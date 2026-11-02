//! The `config.toml` editor — a faithful port of
//! `CmuxCodexConfigEditor` with its `+TOML`, `+Feature`, and `+Trust`
//! extensions (upstream `Packages/macOS/CmuxFoundation/Sources/CmuxFoundation/
//! ConfigPaths/`). A pure value-in/value-out editor: callers supply the
//! current text and receive the rewritten text; the only removable content
//! is what sits inside the cmux-marked blocks.
//!
//! Ported exactly:
//! - marker strings (current + legacy forms) and the previous-line
//!   preservation trick for user-set feature values;
//! - multiline-string masking so edits never corrupt TOML string bodies;
//! - the trust-table key/hash surgical rules (own entries only;
//!   foreign tables inside the marker block are preserved);
//! - the `[features]`-table aware feature flag installation;
//! - empty-features-table cleanup on uninstall.
//!
//! One foundation differs (ADR-0011 amendment 3): upstream trust keys use
//! the real-path of the hooks file; the port uses the constructed path
//! (`fs::canonicalize` yields Windows verbatim paths that differ per
//! machine). Install and uninstall construct the key identically, which is
//! the property the trust checks require.

/// Current and legacy markers (upstream `CmuxCodexConfigEditor+Feature.swift:2-11`).
pub mod markers {
    /// Feature flag (`hooks = true`) block.
    pub const FEATURE_BEGIN: &str =
        "# cmux-codex-hooks-feature-78f1e4ba-66df-4d35-93c1-67fdf1cbb7df begin";
    pub const FEATURE_END: &str =
        "# cmux-codex-hooks-feature-78f1e4ba-66df-4d35-93c1-67fdf1cbb7df end";
    pub const FEATURE_PREVIOUS_PREFIX: &str =
        "# cmux-codex-hooks-feature-78f1e4ba-66df-4d35-93c1-67fdf1cbb7df previous line: ";
    /// Legacy (pre-UUID) feature block markers.
    pub const LEGACY_FEATURE_BEGIN: &str = "# cmux hooks codex feature begin";
    pub const LEGACY_FEATURE_END: &str = "# cmux hooks codex feature end";
    pub const LEGACY_FEATURE_PREVIOUS_PREFIX: &str = "# cmux hooks codex feature previous line: ";

    /// Hook trust block (per-entry `[hooks.state."…"]` + `trusted_hash`).
    pub const TRUST_BEGIN: &str =
        "# cmux-codex-hook-trust-f5cc24da-7a09-4b20-a756-89e7786f6738 begin";
    pub const TRUST_END: &str = "# cmux-codex-hook-trust-f5cc24da-7a09-4b20-a756-89e7786f6738 end";
}

/// One form of the preserved user value (`previous line:` marker).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    fn detect(content: &str) -> Self {
        if content.contains("\r\n") {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

/// TOML basic-string escaping, port of `tomlBasicStringContent`
/// (`+TOML.swift:4-36`).
pub fn toml_basic_string_content(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for scalar in value.chars() {
        match scalar {
            '\u{08}' => escaped.push_str("\\b"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\u{0C}' => escaped.push_str("\\f"),
            '\r' => escaped.push_str("\\r"),
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            c if (c as u32) <= 0x1F || (0x7F..=0x9F).contains(&(c as u32)) => {
                let code = c as u32;
                if code <= 0xFFFF {
                    escaped.push_str(&format!("\\u{code:04X}"));
                } else {
                    escaped.push_str(&format!("\\U{code:08X}"));
                }
            }
            c => escaped.push(c),
        }
    }
    escaped
}

/// Index of the first `#` outside quotes (port of `tomlCommentStart`).
fn toml_comment_start(line: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if let Some(current) = quote {
            if current == '"' {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == current {
                    quote = None;
                }
            } else if character == current {
                quote = None;
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        } else if character == '#' {
            return Some(index);
        }
    }
    None
}

/// Odd number of backslashes directly before `at` (port of `isEscaped`).
fn is_escaped_at(line: &str, at: usize) -> bool {
    let bytes = line.as_bytes();
    let mut backslashes = 0;
    let mut cursor = at;
    while cursor > 0 {
        cursor -= 1;
        if bytes.get(cursor) == Some(&b'\\') {
            backslashes += 1;
        } else {
            break;
        }
    }
    backslashes % 2 == 1
}

fn find_from(haystack: &str, needle: &str, from: usize, content_end: usize) -> Option<usize> {
    if from > content_end || needle.is_empty() {
        return None;
    }
    haystack[from..content_end]
        .find(needle)
        .map(|pos| pos + from)
}

/// Tracks multiline string delimiters across lines: the still-open
/// delimiter after processing `line` (port of
/// `tomlMultilineStringDelimiter`).
fn multiline_delimiter_after(line: &str, starting: Option<&str>) -> Option<String> {
    let content_end = toml_comment_start(line).unwrap_or(line.len());
    let mut cursor = 0usize;
    let mut active: Option<String> = starting.map(str::to_owned);
    while cursor < content_end {
        if let Some(current) = active.clone() {
            match find_from(line, &current, cursor, content_end) {
                Some(end_start) => {
                    if current == "\"\"\"" && is_escaped_at(line, end_start) {
                        cursor = end_start + current.len();
                        continue;
                    }
                    active = None;
                    cursor = end_start + current.len();
                    continue;
                }
                None => return active,
            }
        }
        let basic = find_from(line, "\"\"\"", cursor, content_end);
        let literal = find_from(line, "'''", cursor, content_end);
        let next = match (basic, literal) {
            (Some(basic_pos), Some(literal_pos)) => {
                if basic_pos < literal_pos {
                    ("\"\"\"", basic_pos)
                } else {
                    ("'''", literal_pos)
                }
            }
            (Some(basic_pos), None) => ("\"\"\"", basic_pos),
            (None, Some(literal_pos)) => ("'''", literal_pos),
            (None, None) => return None,
        };
        if next.0 == "\"\"\"" && is_escaped_at(line, next.1) {
            cursor = next.1 + next.0.len();
        } else {
            active = Some(next.0.to_owned());
            cursor = next.1 + next.0.len();
        }
    }
    active
}

const MASKED_PREFIX: char = '\u{001F}';

/// Splits TOML into lines, masking bodies of multiline strings (the mask
/// prefix travels with the line so `unmask` restores the original text).
fn toml_lines(content: &str) -> Vec<String> {
    let raw = split_keep_empty(content);
    let mut lines = Vec::with_capacity(raw.len());
    let mut delimiter: Option<String> = None;
    for line in raw {
        let masked = delimiter.is_some();
        lines.push(if masked {
            format!("{MASKED_PREFIX}{line}")
        } else {
            line.clone()
        });
        delimiter = multiline_delimiter_after(&line, delimiter.as_deref());
    }
    lines
}

/// Splits without dropping a trailing chunk and without dropping empty
/// lines; a trailing newline in the source becomes a final empty entry
/// (matched by upstream's `CmuxConfigLines` + `joined` round trip).
fn split_keep_empty(content: &str) -> Vec<String> {
    let mut source = content;
    let mut trailing_newline = false;
    let mut crlf = false;
    if content.ends_with('\n') {
        trailing_newline = true;
        if content.ends_with("\r\n") {
            crlf = true;
        }
        source = &content[..content.len().saturating_sub(if crlf { 2 } else { 1 })];
    }
    let mut lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
    if crlf {
        lines = lines
            .into_iter()
            .map(|l| l.strip_suffix('\r').map(str::to_owned).unwrap_or(l))
            .collect();
    }
    if trailing_newline {
        lines.push(String::new());
    }
    lines
}

/// Reconstructs text with the original line-ending style, unmasking. When
/// the source ended with a newline, the output does too (upstream
/// `CmuxConfigLines.joined` round trip).
fn toml_content(lines: &[String], ending: LineEnding, source_had_trailing_newline: bool) -> String {
    let unmasked: Vec<String> = lines
        .iter()
        .map(|line| {
            line.strip_prefix(MASKED_PREFIX)
                .map(str::to_owned)
                .unwrap_or_else(|| line.clone())
        })
        .collect();
    let mut content = unmasked.join(ending.as_str());
    if source_had_trailing_newline && !content.ends_with(ending.as_str()) {
        content.push_str(ending.as_str());
    }
    content
}

fn trimmed(line: &str) -> &str {
    line.trim()
}

fn unmask(line: &str) -> &str {
    line.strip_prefix(MASKED_PREFIX).unwrap_or(line)
}

fn without_comment(line: &str) -> &str {
    match toml_comment_start(line) {
        Some(index) => &line[..index],
        None => line,
    }
}

// ---------------------------------------------------------------- TOML keys

/// `^\s*key\s*=` (regex-free port of `tomlLineDefinesKey`).
fn line_defines_key(key: &str, line: &str) -> bool {
    assignment_after_key(key, trimmed(line)).is_some()
}

/// The value text between `key =` and a whole-line comment.
fn assignment_after_key<'a>(key: &str, line: &'a str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let before_comment = toml_comment_start(rest).unwrap_or(rest.len());
    Some(rest[..before_comment].trim())
}

/// `^\s*key\s*=\s*true(\s*#.*)?$` (port of `tomlLineDefinesTrueKey`).
fn line_defines_true_key(key: &str, line: &str) -> bool {
    assignment_after_key(key, trimmed(line))
        .map(value_is_true)
        .unwrap_or(false)
}

/// `^\s*features\s*\.\s*key\s*=` (port of `tomlLineDefinesDottedFeaturesKey`).
fn line_defines_dotted_features_key(key: &str, line: &str) -> bool {
    dotted_assignment_after_features(trimmed(line))
        .map(|(name, _)| name == key)
        .unwrap_or(false)
}

/// `^\s*features\s*\.\s*key\s*=\s*true(\s*#.*)?$`.
fn line_defines_dotted_features_true_key(key: &str, line: &str) -> bool {
    dotted_assignment_after_features(trimmed(line))
        .map(|(name, value)| name == key && value_is_true(value))
        .unwrap_or(false)
}

/// Any `features.<key> =` line (port of `tomlLineDefinesAnyDottedFeaturesKey`).
fn line_defines_any_dotted_features_key(line: &str) -> bool {
    dotted_assignment_after_features(trimmed(line)).is_some()
}

/// Parses `features . <key> = <value-up-to-comment>`; key must be a bare
/// TOML-key run (`[^=\s]+`) per upstream's pattern.
fn dotted_assignment_after_features(line: &str) -> Option<(&str, &str)> {
    let without = without_comment(line);
    let trimmed_line = without.trim();
    let rest = trimmed_line.strip_prefix("features").map(str::trim)?;
    let rest = rest.strip_prefix('.')?;
    let rest = rest.trim();
    let name_end = rest.find(|c: char| c.is_whitespace() || c == '=')?;
    let name = &rest[..name_end];
    if name.is_empty() || name.contains('=') {
        return None;
    }
    let after_name = rest[name_end..].trim_start();
    let value = after_name.strip_prefix('=')?.trim();
    Some((name, value))
}

fn value_is_true(value: &str) -> bool {
    if value == "true" {
        return true;
    }
    // `true` followed only by whitespace and possibly a comment;
    // `true-ish` values like `truex` must not match.
    value.starts_with("true") && {
        let after = value["true".len()..].trim();
        after.is_empty() || after.starts_with('#')
    }
}

fn line_is_table(name: &str, line: &str) -> bool {
    let t = without_comment(trimmed(line)).trim();
    let Some(inner) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    let inner = inner.trim();
    match inner.strip_prefix(name) {
        Some(rest) => rest.trim().is_empty(),
        None => false,
    }
}

/// Port of `tomlLineIsAnyTableHeader`: plain/quoted/dotted keys, `[t]` or
/// `[[t]]`.
fn line_is_any_table_header(line: &str) -> bool {
    let t = without_comment(trimmed(line)).trim();
    let (array_tokens, rest) = match t.starts_with("[[") {
        true => (
            true,
            t.strip_prefix("[[").and_then(|s| s.strip_suffix("]]")),
        ),
        false => (false, t.strip_prefix('[').and_then(|s| s.strip_suffix(']'))),
    };
    let Some(inner) = rest else {
        return false;
    };
    if inner.is_empty() {
        return false;
    }
    if array_tokens && inner.contains('[') {
        return false;
    }
    inner.split('.').all(is_toml_key_like)
}

fn is_toml_key_like(segment: &str) -> bool {
    let segment = segment.trim();
    if segment.is_empty() {
        return false;
    }
    if segment.len() >= 2
        && ((segment.starts_with('"') && segment.ends_with('"'))
            || (segment.starts_with('\'') && segment.ends_with('\'')))
    {
        return true;
    }
    segment
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn table_end_index(lines: &[String], after: usize) -> usize {
    let mut index = after + 1;
    while index < lines.len() {
        if line_is_any_table_header(&lines[index]) {
            return index;
        }
        index += 1;
    }
    lines.len()
}

// ------------------------------------------------------------- trust tables

/// One cmux-owned codex hook-trust table (upstream `HookTrustEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustEntry {
    /// The stable, UNescaped TOML key for the hook table.
    pub key: String,
    /// `"sha256:<hex>"` (unescaped form).
    pub trusted_hash: String,
}

/// Extracts the escaped key from a `[hooks.state."…"]` line (port of
/// `codexHookTrustTableEscapedKey`).
fn trust_table_escaped_key(line: &str) -> Option<String> {
    let t = without_comment(trimmed(line)).trim();
    // The regex matches a single-bracket table header only.
    let inner = t.strip_prefix('[').and_then(|s| s.strip_suffix(']'))?;
    if inner.starts_with('[') || inner.ends_with(']') {
        return None;
    }
    let inner = inner.trim();
    let inner = inner.strip_prefix("hooks")?.trim();
    if !inner.starts_with('.') {
        return None;
    }
    let inner = inner[1..].trim();
    let inner = inner.strip_prefix("state")?.trim();
    if !inner.starts_with('.') {
        return None;
    }
    let inner = inner[1..].trim();
    let name = inner.strip_prefix('"')?;
    // The value inside the quotes, escapes intact (comparison uses escaped
    // forms on both sides).
    let last = name.rfind('"')?;
    if last == 0 {
        return None;
    }
    Some(name[..last].to_owned())
}

fn line_is_trust_block_table_boundary(line: &str) -> bool {
    trust_table_escaped_key(line).is_some() || line_is_any_table_header(line)
}

/// The `trusted_hash` value inside a hook-state table (port of
/// `codexHookTrustTrustedHash`).
fn trust_table_trusted_hash(table: &[&str]) -> Option<String> {
    for line in table {
        let t = trimmed(line);
        let equals = t.find('=')?;
        let key = t[..equals].trim();
        if key != "trusted_hash" {
            continue;
        }
        let value = t[equals + 1..].trim();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            return Some(value[1..value.len() - 1].to_owned());
        }
    }
    None
}

/// Removes cmux-owned tables from inside a trust block, preserving
/// everything else (port of `codexHookTrustBlockUnownedLines`).
fn trust_block_unowned_lines(
    block_lines: &[&str],
    escaped_keys: &std::collections::HashSet<String>,
    escaped_key_prefixes: &std::collections::HashSet<String>,
    trusted_hashes: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut preserved: Vec<String> = Vec::new();
    let mut index = 0usize;
    while index < block_lines.len() {
        let line = block_lines[index];
        if let Some(escaped_key) = trust_table_escaped_key(line) {
            let start = index;
            index += 1;
            while index < block_lines.len()
                && !line_is_trust_block_table_boundary(block_lines[index])
            {
                index += 1;
            }
            let table = &block_lines[start..index];
            let hash_in_table = trust_table_trusted_hash(table);
            let removed = escaped_keys.contains(&escaped_key)
                || hash_in_table.is_some_and(|hash| {
                    trusted_hashes.contains(&hash)
                        && escaped_key_prefixes
                            .iter()
                            .any(|prefix| escaped_key.starts_with(prefix))
                });
            if !removed {
                preserved.extend(table.iter().map(|l| (*l).to_owned()));
            }
            continue;
        }
        if !line_is_any_table_header(line) {
            // Marker drift can capture user config lines; only cmux-owned
            // hook trust tables are safely discarded.
            preserved.push(line.to_owned());
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < block_lines.len() && !line_is_trust_block_table_boundary(block_lines[index]) {
            index += 1;
        }
        preserved.extend(block_lines[start..index].iter().map(|l| (*l).to_owned()));
    }
    preserved
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustResult {
    NotFound,
    Removed,
    Malformed,
}

/// Replaces trust blocks in place, removing owned entries and preserving
/// foreign content (port of `removeCmuxCodexHookTrustBlock`).
fn remove_trust_block(
    lines: &mut Vec<String>,
    escaped_keys: &std::collections::HashSet<String>,
    escaped_key_prefixes: &std::collections::HashSet<String>,
    trusted_hashes: &std::collections::HashSet<String>,
) -> TrustResult {
    let mut malformed = false;
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index] != markers::TRUST_BEGIN {
            index += 1;
            continue;
        }
        match lines[index..]
            .iter()
            .position(|l| l.as_str() == markers::TRUST_END)
            .map(|offset| index + offset)
        {
            Some(end) => {
                blocks.push((index, end));
                index = end + 1;
            }
            None => {
                malformed = true;
                break;
            }
        }
    }
    if malformed {
        return TrustResult::Malformed;
    }
    if blocks.is_empty() {
        return TrustResult::NotFound;
    }
    for (start, end) in blocks.into_iter().rev() {
        let refs: Vec<&str> = lines[start + 1..end].iter().map(|l| l.as_str()).collect();
        let preserved =
            trust_block_unowned_lines(&refs, escaped_keys, escaped_key_prefixes, trusted_hashes);
        lines.splice(start..=end, preserved);
    }
    TrustResult::Removed
}

/// Removes trust tables anywhere in the file (port of
/// `removeCodexHookTrustTables`): used during install to replace stale
/// entries whose blocks were already reconstructed.
fn remove_trust_tables(lines: &mut Vec<String>, escaped_keys: &std::collections::HashSet<String>) {
    if escaped_keys.is_empty() {
        return;
    }
    let mut index = 0;
    while index < lines.len() {
        let Some(escaped_key) = trust_table_escaped_key(&lines[index]) else {
            index += 1;
            continue;
        };
        if !escaped_keys.contains(&escaped_key) {
            index += 1;
            continue;
        }
        let end = trust_table_end_index(lines, index);
        lines.splice(index..end, Vec::new());
    }
}

fn trust_table_end_index(lines: &[String], table_start: usize) -> usize {
    let mut index = table_start + 1;
    while index < lines.len() {
        if line_is_trust_block_table_boundary(&lines[index]) {
            return index;
        }
        index += 1;
    }
    lines.len()
}

fn strip_malformed_trust_marker(lines: &mut Vec<String>) {
    lines.retain(|line| line.as_str() != markers::TRUST_BEGIN);
}

// ------------------------------------------------------------ feature block

fn is_feature_begin(line: &str) -> bool {
    line == markers::FEATURE_BEGIN || line == markers::LEGACY_FEATURE_BEGIN
}

fn is_feature_end(line: &str) -> bool {
    line == markers::FEATURE_END || line == markers::LEGACY_FEATURE_END
}

fn feature_previous_line(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix(markers::FEATURE_PREVIOUS_PREFIX) {
        return Some(rest.to_owned());
    }
    if let Some(rest) = line.strip_prefix(markers::LEGACY_FEATURE_PREVIOUS_PREFIX) {
        return Some(rest.to_owned());
    }
    None
}

fn is_codex_hooks_feature_setting(line: &str) -> bool {
    line_defines_true_key("hooks", line) || line_defines_dotted_features_true_key("hooks", line)
}

/// Removes feature blocks, restoring preserved previous lines (port of
/// `removeCmuxCodexHooksFeatureBlock`).
fn remove_feature_block(lines: &mut Vec<String>) {
    let mut cursor = 0;
    while cursor < lines.len() {
        if !is_feature_begin(&lines[cursor]) {
            cursor += 1;
            continue;
        }
        if let Some(offset) = lines[cursor..]
            .iter()
            .position(|l| is_feature_end(l.as_str()))
        {
            let last = cursor + offset;
            let previous: Vec<String> = lines[cursor..=last]
                .iter()
                .filter_map(|l| feature_previous_line(l.as_str()))
                .collect();
            lines.splice(cursor..=last, previous);
            continue;
        }
        // Malformed: begin without end — strip marker, previous line, and
        // the setting line.
        let mut block_end = cursor + 1;
        let mut previous_lines: Vec<String> = Vec::new();
        if block_end < lines.len() {
            if let Some(previous) = feature_previous_line(lines[block_end].as_str()) {
                previous_lines.push(previous);
                block_end += 1;
            }
        }
        if block_end < lines.len() && is_codex_hooks_feature_setting(lines[block_end].as_str()) {
            block_end += 1;
        }
        lines.splice(cursor..block_end, previous_lines);
    }
}

fn feature_lines(setting_line: &str, previous_line: Option<&str>) -> Vec<String> {
    let mut lines = vec![markers::FEATURE_BEGIN.to_owned()];
    if let Some(previous) = previous_line {
        lines.push(format!("{}{}", markers::FEATURE_PREVIOUS_PREFIX, previous));
    }
    lines.push(setting_line.to_owned());
    lines.push(markers::FEATURE_END.to_owned());
    lines
}

fn remove_empty_features_table(lines: &mut Vec<String>) {
    let Some(features_start) = lines
        .iter()
        .position(|line| line_is_table("features", line.as_str()))
    else {
        return;
    };
    let features_end = table_end_index(lines, features_start);
    let has_content = (features_start + 1..features_end).any(|index| {
        let t = trimmed(lines[index].as_str());
        !t.is_empty() && !t.starts_with('#')
    });
    if has_content {
        return;
    }
    lines.splice(features_start..features_end, Vec::new());
    if features_start == lines.len()
        && features_start > 0
        && trimmed(&lines[features_start - 1]).is_empty()
    {
        lines.remove(features_start - 1);
    } else if features_start > 0
        && features_start < lines.len()
        && trimmed(&lines[features_start - 1]).is_empty()
        && trimmed(&lines[features_start]).is_empty()
    {
        lines.remove(features_start);
    }
}

/// Installs the `hooks = true` feature (port of `installingHooksFeature`).
/// A user-set value is REPLACED by the marker block, which records the
/// user's line after `previous line:` so an uninstall can restore it.
fn installing_hooks_feature(lines: &mut Vec<String>) {
    remove_feature_block(lines);
    lines.retain(|line| !line_defines_key("codex_hooks", line.as_str()));
    lines.retain(|line| !line_defines_dotted_features_key("codex_hooks", line.as_str()));

    if let Some(features_start) = lines
        .iter()
        .position(|line| line_is_table("features", line.as_str()))
    {
        let features_end = table_end_index(lines, features_start);
        let hooks_index = (features_start + 1..features_end)
            .find(|&i| line_defines_key("hooks", lines[i].as_str()));
        match hooks_index {
            Some(index) => {
                if !line_defines_true_key("hooks", lines[index].as_str()) {
                    let previous = lines[index]
                        .strip_prefix(MASKED_PREFIX)
                        .unwrap_or(&lines[index])
                        .to_owned();
                    let block = feature_lines("hooks = true", Some(&previous));
                    lines.splice(index..=index, block);
                }
            }
            None => {
                lines.splice(
                    features_start + 1..features_start + 1,
                    feature_lines("hooks = true", None),
                );
            }
        }
    } else if let Some(dotted_index) = lines
        .iter()
        .position(|line| line_defines_dotted_features_key("hooks", line.as_str()))
    {
        if !line_defines_dotted_features_true_key("hooks", lines[dotted_index].as_str()) {
            let previous = lines[dotted_index]
                .strip_prefix(MASKED_PREFIX)
                .unwrap_or(&lines[dotted_index])
                .to_owned();
            let block = feature_lines("features.hooks = true", Some(&previous));
            lines.splice(dotted_index..=dotted_index, block);
        }
    } else if let Some(first_dotted) = lines
        .iter()
        .position(|line| line_defines_any_dotted_features_key(line.as_str()))
    {
        lines.splice(
            first_dotted..first_dotted,
            feature_lines("features.hooks = true", None),
        );
    } else {
        if !lines.is_empty() && !unmask(lines.last().expect("nonempty")).trim().is_empty() {
            lines.push(String::new());
        }
        lines.push("[features]".to_owned());
        lines.extend(feature_lines("hooks = true", None));
    }
}

fn ensures_trailing_blank_line(lines: &mut Vec<String>) {
    if !lines.is_empty() && !unmask(lines.last().expect("nonempty")).trim().is_empty() {
        lines.push(String::new());
    }
}

// ---------------------------------------------------------------- public API

/// Installs the hooks feature and trust tables in the upstream order
/// (trust clean → feature flag → trust block insert) — port of
/// `installingHooks` (+Feature +Trust).
pub struct InstallResult {
    pub content: String,
    pub installed_trust: bool,
}

pub fn installing_hooks(
    existing_content: &str,
    trust_entries: &[TrustEntry],
    removing_key_prefixes: &[String],
    removing_trusted_hashes: &[String],
) -> InstallResult {
    let escaped_key_prefixes: std::collections::HashSet<String> = removing_key_prefixes
        .iter()
        .map(|p| toml_basic_string_content(p))
        .collect();
    let escapes: std::collections::HashSet<String> = trust_entries
        .iter()
        .map(|e| toml_basic_string_content(&e.key))
        .collect();
    let all_hashes: std::collections::HashSet<String> = trust_entries
        .iter()
        .map(|e| e.trusted_hash.clone())
        .chain(removing_trusted_hashes.iter().cloned())
        .collect();

    let mut lines = toml_lines(existing_content);
    // Phase 1: clean matching trust content out of the trust blocks.
    let trust_removed =
        remove_trust_block(&mut lines, &escapes, &escaped_key_prefixes, &all_hashes);
    if trust_removed == TrustResult::Malformed {
        strip_malformed_trust_marker(&mut lines);
    }
    // Phase 2: feature flag.
    installing_hooks_feature(&mut lines);
    // Phase 3: attach the trust block.
    let installed = installing_hook_trust_block(
        &mut lines,
        trust_entries,
        &escaped_key_prefixes,
        &all_hashes,
    );
    InstallResult {
        content: toml_content(
            &lines,
            LineEnding::detect(existing_content),
            existing_content.ends_with('\n'),
        ),
        installed_trust: installed,
    }
}

fn installing_hook_trust_block(
    lines: &mut Vec<String>,
    entries: &[TrustEntry],
    escaped_key_prefixes: &std::collections::HashSet<String>,
    trusted_hashes: &std::collections::HashSet<String>,
) -> bool {
    let escaped_keys: std::collections::HashSet<String> = entries
        .iter()
        .map(|e| toml_basic_string_content(&e.key))
        .collect();
    let trust_removed =
        remove_trust_block(lines, &escaped_keys, escaped_key_prefixes, trusted_hashes);
    if trust_removed == TrustResult::Malformed {
        strip_malformed_trust_marker(lines);
    }
    if entries.is_empty() {
        return false;
    }
    remove_trust_tables(lines, &escaped_keys);
    ensures_trailing_blank_line(lines);
    lines.push(markers::TRUST_BEGIN.to_owned());
    for entry in entries {
        lines.push(format!(
            "[hooks.state.\"{}\"]",
            toml_basic_string_content(&entry.key)
        ));
        lines.push(format!(
            "trusted_hash = \"{}\"",
            toml_basic_string_content(&entry.trusted_hash)
        ));
    }
    lines.push(markers::TRUST_END.to_owned());
    true
}

/// Removes the feature flag, the trust block, matching trust tables, and
/// the legacy `codex_hooks` keys (port of `uninstallingHooks`).
pub fn uninstalling_hooks(
    existing_content: &str,
    entries: &[TrustEntry],
    removing_key_prefixes: &[String],
    removing_trusted_hashes: &[String],
) -> String {
    let escaped_keys: std::collections::HashSet<String> = entries
        .iter()
        .map(|e| toml_basic_string_content(&e.key))
        .collect();
    let all_hashes: std::collections::HashSet<String> = entries
        .iter()
        .map(|e| e.trusted_hash.clone())
        .chain(removing_trusted_hashes.iter().cloned())
        .collect();
    let escaped_key_prefixes: std::collections::HashSet<String> = removing_key_prefixes
        .iter()
        .map(|p| toml_basic_string_content(p))
        .collect();

    let mut lines = toml_lines(existing_content);
    remove_feature_block(&mut lines);
    let trust_removed = remove_trust_block(
        &mut lines,
        &escaped_keys,
        &escaped_key_prefixes,
        &all_hashes,
    );
    if trust_removed == TrustResult::Malformed {
        strip_malformed_trust_marker(&mut lines);
    }
    remove_trust_tables(&mut lines, &escaped_keys);
    lines.retain(|line| !line_defines_key("codex_hooks", line.as_str()));
    lines.retain(|line| !line_defines_dotted_features_key("codex_hooks", line.as_str()));
    remove_empty_features_table(&mut lines);
    // Uninstall restores the pre-cmux state: blocks that were appended
    // after the file's trailing blank leave stragglers once removed, so
    // trailing blank lines are dropped here and the terminator is
    // re-added from the source's own convention.
    while lines
        .last()
        .map(|l| unmask(l).trim().is_empty())
        .unwrap_or(false)
    {
        lines.pop();
    }
    toml_content(
        &lines,
        LineEnding::detect(existing_content),
        existing_content.ends_with('\n'),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(key: &str, command: &str) -> (Vec<TrustEntry>, Vec<String>) {
        // One trust entry whose hash is the canonical hash of the command.
        let hash_value = crate::hash::codex_command_hook_hash("stop", None, command, 5, None);
        (
            vec![TrustEntry {
                key: key.to_owned(),
                trusted_hash: hash_value.clone(),
            }],
            vec![hash_value],
        )
    }

    #[test]
    fn fresh_install_writes_the_feature_and_the_trust_block() {
        let (entries, _) = sample_entry(
            r"C:\tmp\hooks.json:stop:0:0",
            r#""C:\x\cmux.exe" hooks enqueue codex stop"#,
        );
        let result = installing_hooks("", &entries, &["C:\\tmp\\hooks.json:".to_owned()], &[]);
        assert!(result.installed_trust);
        // Fresh files get a [features] table then the feature block.
        assert!(result.content.contains("[features]"), "{}", result.content);
        assert!(
            result.content.contains("hooks = true"),
            "{}",
            result.content
        );
        // The trust key is basic-string escaped (backslashes doubled).
        assert!(
            result
                .content
                .contains("[hooks.state.\"C:\\\\tmp\\\\hooks.json:stop:0:0\"]"),
            "{}",
            result.content
        );
        assert!(result.content.contains("trusted_hash = "));
        // On a fresh (empty) file the feature table leads; the marker block
        // follows inside it.
        assert!(
            result.content.trim_start().starts_with("[features]"),
            "{}",
            result.content
        );
    }

    #[test]
    fn reinstall_is_idempotent() {
        let (entries, _) = sample_entry(r"C:\tmp\hooks.json:stop:0:0", "cmd");
        let once = installing_hooks("", &entries, &[], &[]);
        let twice = installing_hooks(&once.content, &entries, &[], &[]);
        assert!(twice.installed_trust, "entries remain installable");
        assert_eq!(once.content, twice.content);
    }

    #[test]
    fn user_feature_value_is_replaced_and_restored() {
        // `hooks = false` (user-set): install replaces the line and records
        // the previous value; uninstall restores it byte-for-byte.
        let source = "model = \"gpt-5\"\n[features]\nhooks = false\n";
        let (entries, hashes) = sample_entry(r"C:\tmp\hooks.json:stop:0:0", "cmd");
        let installed =
            installing_hooks(source, &entries, &["C:\\tmp\\hooks.json:".to_owned()], &[]);
        assert!(
            installed.content.contains("previous line: hooks = false"),
            "previous value recorded: {}",
            installed.content
        );
        assert!(installed.content.contains("hooks = true"));
        assert!(!installed.content.contains("\nhooks = false\n"));
        let removed = uninstalling_hooks(
            &installed.content,
            &entries,
            &["C:\\tmp\\hooks.json:".to_owned()],
            &hashes,
        );
        assert_eq!(removed, source);
    }

    #[test]
    fn foreign_trust_tables_inside_the_block_are_preserved() {
        let source = "\n\
# cmux-codex-hook-trust-f5cc24da-7a09-4b20-a756-89e7786f6738 begin\n\
[hooks.state.\"foreign-key\"]\ntrusted_hash = \"keep-me\"\n\
[hooks.state.\"C:\\\\old\\\\hooks.json:stop:0:0\"]\ntrusted_hash = \"gone\"\n\
# cmux-codex-hook-trust-f5cc24da-7a09-4b20-a756-89e7786f6738 end\n";
        let entries = vec![TrustEntry {
            key: r"C:\new\hooks.json:stop:0:0".to_owned(),
            trusted_hash: "sha256:abc".to_owned(),
        }];
        let result = installing_hooks(source, &entries, &[r"C:\old\hooks.json:".to_owned()], &[]);
        // The foreign table survives (whole-table form preserved); the
        // stale path-prefixed entry whose hash matches the removal hash is
        // dropped... it does NOT match "sha256:abc", but its KEY does not
        // match escapedKeys either — upstream requires the HASH to match
        // for prefix removals, so without a matching hash it must stay.
        assert!(result.content.contains("foreign-key"), "{}", result.content);
        assert!(result.content.contains("keep-me"));
        assert!(result.content.contains("sha256:abc"));
    }

    #[test]
    fn stale_entries_drop_when_the_story_has_both_key_and_hash() {
        let stale_hash = crate::hash::codex_command_hook_hash("stop", None, "old-cmd", 5, None);
        let source = format!(
            "# cmux-codex-hook-trust-f5cc24da-7a09-4b20-a756-89e7786f6738 begin\n\
[hooks.state.\"C:\\\\old\\\\hooks.json:stop:0:0\"]\ntrusted_hash = \"{stale_hash}\"\n\
# cmux-codex-hook-trust-f5cc24da-7a09-4b20-a756-89e7786f6738 end\n"
        );
        let entries = vec![TrustEntry {
            key: r"C:\new\hooks.json:stop:0:0".to_owned(),
            trusted_hash: "sha256:current".to_owned(),
        }];
        let result = installing_hooks(
            &source,
            &entries,
            &[r"C:\old\hooks.json:".to_owned()],
            &[stale_hash],
        );
        assert!(!result.content.contains("old-cmd"), "{}", result.content);
        assert!(result.content.contains("sha256:current"));
    }

    #[test]
    fn multiline_string_bodies_are_never_touched() {
        let source = "multiline = \"\"\"\n\
[features]\n\
[hooks.state.\"spoof\"]\n\
trusted_hash = \"nope\"\n\
\"\"\"\nmotd = \"x\"\n";
        let (entries, _) = sample_entry(r"C:\tmp\hooks.json:stop:0:0", "cmd");
        let result = installing_hooks(source, &entries, &[r"C:\tmp\hooks.json:".to_owned()], &[]);
        // The spoof lines remain verbatim inside the string body.
        assert!(
            result.content.contains("[hooks.state.\"spoof\"]"),
            "{}",
            result.content
        );
        let body = result
            .content
            .split("multiline = \"\"\"\n")
            .nth(1)
            .and_then(|rest| rest.split("\n\"\"\"").next())
            .expect("multiline string intact");
        assert_eq!(
            body,
            "[features]\n[hooks.state.\"spoof\"]\ntrusted_hash = \"nope\""
        );
        // Real blocks are still installed.
        assert!(result.content.contains(multiline_feature_marker()));
    }

    fn multiline_feature_marker() -> &'static str {
        markers::FEATURE_BEGIN
    }

    #[test]
    fn dotted_features_keys_get_the_dotted_form() {
        let source = "features.other = true\n";
        let (entries, _) = sample_entry("k", "cmd");
        let result = installing_hooks(source, &entries, &[], &[]);
        assert!(
            result.content.contains("features.hooks = true"),
            "{}",
            result.content
        );
        let lines: Vec<&str> = result.content.lines().collect();
        let marker_count = lines
            .iter()
            .filter(|l| **l == markers::FEATURE_BEGIN)
            .count();
        assert_eq!(marker_count, 1);
    }

    #[test]
    fn uninstall_removes_legacy_codex_hooks_keys_and_empty_features() {
        let source = "[features]\ncodex_hooks = true\n";
        let removed = uninstalling_hooks(source, &[], &[], &[]);
        assert!(
            !removed.contains("codex_hooks"),
            "legacy key removed: {removed:?}"
        );
        assert!(
            !removed.contains("[features]"),
            "empty features table removed"
        );
    }

    #[test]
    fn crlf_files_keep_their_endings() {
        let source = "a = 1\r\n";
        let (entries, _) = sample_entry(r"C:\tmp\hooks.json:stop:0:0", "cmd");
        let result = installing_hooks(source, &entries, &[], &[]);
        // Every line ending must be CR-LF (no bare LF sneaks in).
        let without_pairs = result.content.replace("\r\n", "");
        assert!(
            !without_pairs.contains('\n'),
            "bare LF found: {:?}",
            without_pairs
        );
        assert!(result.content.ends_with("\r\n"));
    }
}
