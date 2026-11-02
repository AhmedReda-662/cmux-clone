//! The Claude Code settings-document generator (M3.3b) — the port's
//! `emitClaudeWrapperInjectSettings` (upstream
//! `CLI/CMUXCLI+ClaudeHookSettings.swift:9-151`).
//!
//! Upstream never edits `~/.claude/settings.json`; the wrapper asks the CLI
//! for this document, stores it under cmux's state dir, and execs the real
//! `claude` with `--settings <file>` (verified: claude merges `--settings`
//! over the other settings sources — it is "additional" settings, not a
//! replacement).
//!
//! Divergence (ADR-0011 amendment 4, documented): upstream's hook commands
//! are **shell-form** strings (`"${CMUX_CLAUDE_HOOK_CMUX_BIN:-cmux}" hooks
//! …`) wrapped in a spool producer with a POSIX shell guard. On Windows the
//! documented hook execution path for shell form is Git Bash-or-PowerShell,
//! so the POSIX guard is not portable. The port emits **exec-form**
//! handlers instead — `command` = the absolute `cmux.exe` path plus an
//! `args` array — which Claude Code spawns directly "with no shell" on all
//! platforms (hooks reference, exec form and shell form). The admission
//! gating the guard performed (`CMUX_SURFACE_ID` presence,
//! `CMUX_CLAUDE_HOOKS_DISABLED != 1`, fail-open `{}`) lives in the enqueue
//! client, exactly as for codex in slice A. Same events, same payloads,
//! same subcommands; the spool fast path is not reproduced (M4 contract
//! item, upstream `AgentHookSpoolProducer`).
//!
//! The event table mirrors upstream one-for-one, including the async
//! machinery: `stop` also runs the queued `feed` group and async
//! `auto-name`; `inbox-wait` (async + `asyncRewake`, 86400 s — verified
//! native in claude.exe 2.1.302: "runs in the background and wakes Claude
//! on exit code 2 … implies async") rides SessionStart/Stop/StopFailure;
//! `inbox-drain` trails every prompt submit; PreToolUse carries the
//! `CronCreate` guard and the queued feed; `PermissionRequest` keeps the
//! direct `hooks feed --source claude` decision hook. The port-side CLI
//! verbs those non-queued commands point at are fail-open no-ops until the
//! agent-messages/team features land (M4+); every admission command runs
//! through `hooks enqueue claude <sub>`, which fails open as always.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The queued-admission timeout in whole seconds (upstream
/// `agentHookDeclaredTimeoutSeconds` — the same shared constant the codex
/// nested format writes as `timeout: 5`).
pub const DECLARED_TIMEOUT_SECONDS: i64 = 5;

/// `hooks claude auto-name` runs detached (upstream: timeout 120, async).
pub const AUTO_NAME_TIMEOUT_SECONDS: i64 = 120;

/// "Claude doesn't enforce timeouts on background hooks; this bounds the
/// wait hook anyway if a future version does" (upstream comment).
pub const INBOX_WAIT_TIMEOUT_SECONDS: i64 = 86_400;

/// `hooks feed --source claude` is a synchronous decision hook (upstream
/// keeps it direct with timeout 125).
pub const FEED_TIMEOUT_SECONDS: i64 = 125;

/// The exec-form handler for one queued admission event: invoke the CLI's
/// enqueue verb, which performs the surface-id/disable-var gating and
/// fails open (ADR-0011 amendment 4).
fn enqueue_handler(cmux_bin: &Path, subcommand: &str, timeout: i64) -> serde_json::Value {
    serde_json::json!({
        "args": ["hooks", "enqueue", "claude", subcommand],
        "command": cmux_bin.display().to_string(),
        "timeout": timeout,
        "type": "command",
    })
}

/// One matcher group in the claude settings schema
/// (`{"matcher": "", "hooks": [handler]}`), key order irrelevant — the
/// document is serialized sorted (upstream `.sortedKeys`).
fn group(matcher: &str, handler: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "hooks": [handler], "matcher": matcher })
}

/// A non-queued verb handler (`hooks claude <verb>` / `hooks feed`).
fn verb_handler(
    cmux_bin: &Path,
    args: &[&str],
    timeout: i64,
    is_async: bool,
    is_async_rewake: bool,
) -> serde_json::Value {
    let mut handler = serde_json::json!({
        "args": args,
        "command": cmux_bin.display().to_string(),
        "timeout": timeout,
        "type": "command",
    });
    if is_async {
        handler["async"] = serde_json::Value::Bool(true);
    }
    if is_async_rewake {
        handler["asyncRewake"] = serde_json::Value::Bool(true);
    }
    handler
}

/// The complete cmux-owned claude settings object, upstream's exact event
/// table with the port's exec-form commands. Serialization is compact with
/// sorted keys and unescaped slashes (`[.sortedKeys, .withoutEscapingSlashes]`
/// upstream; serde_json's BTreeMap-backed objects sort and never escape `/`).
pub fn settings_doc_json(cmux_bin: &Path) -> serde_json::Value {
    let hooks = [
        (
            "SessionStart",
            vec![
                group(
                    "",
                    enqueue_handler(cmux_bin, "session-start", DECLARED_TIMEOUT_SECONDS),
                ),
                group(
                    "",
                    verb_handler(
                        cmux_bin,
                        &["hooks", "claude", "inbox-wait"],
                        INBOX_WAIT_TIMEOUT_SECONDS,
                        true,
                        true,
                    ),
                ),
            ],
        ),
        (
            "Stop",
            vec![
                group(
                    "",
                    enqueue_handler(cmux_bin, "stop", DECLARED_TIMEOUT_SECONDS),
                ),
                group(
                    "",
                    enqueue_handler(cmux_bin, "feed", DECLARED_TIMEOUT_SECONDS),
                ),
                group(
                    "",
                    verb_handler(
                        cmux_bin,
                        &["hooks", "claude", "auto-name"],
                        AUTO_NAME_TIMEOUT_SECONDS,
                        true,
                        false,
                    ),
                ),
                group(
                    "",
                    verb_handler(
                        cmux_bin,
                        &["hooks", "claude", "inbox-wait"],
                        INBOX_WAIT_TIMEOUT_SECONDS,
                        true,
                        true,
                    ),
                ),
            ],
        ),
        (
            "StopFailure",
            vec![
                group(
                    "",
                    enqueue_handler(cmux_bin, "stop", DECLARED_TIMEOUT_SECONDS),
                ),
                group(
                    "",
                    verb_handler(
                        cmux_bin,
                        &["hooks", "claude", "inbox-wait"],
                        INBOX_WAIT_TIMEOUT_SECONDS,
                        true,
                        true,
                    ),
                ),
            ],
        ),
        (
            "SessionEnd",
            vec![group(
                "",
                enqueue_handler(cmux_bin, "session-end", DECLARED_TIMEOUT_SECONDS),
            )],
        ),
        (
            "Notification",
            vec![group(
                "",
                enqueue_handler(cmux_bin, "notification", DECLARED_TIMEOUT_SECONDS),
            )],
        ),
        (
            "UserPromptSubmit",
            vec![
                group(
                    "",
                    enqueue_handler(cmux_bin, "prompt-submit", DECLARED_TIMEOUT_SECONDS),
                ),
                group(
                    "",
                    verb_handler(
                        cmux_bin,
                        &["hooks", "claude", "inbox-drain"],
                        DECLARED_TIMEOUT_SECONDS,
                        false,
                        false,
                    ),
                ),
            ],
        ),
        (
            "SubagentStop",
            vec![group(
                "",
                enqueue_handler(cmux_bin, "feed", DECLARED_TIMEOUT_SECONDS),
            )],
        ),
        (
            "PreToolUse",
            vec![
                group(
                    "CronCreate",
                    verb_handler(
                        cmux_bin,
                        &["hooks", "claude", "cron-create-guard"],
                        DECLARED_TIMEOUT_SECONDS,
                        false,
                        false,
                    ),
                ),
                group(
                    "",
                    enqueue_handler(cmux_bin, "pre-tool-use", DECLARED_TIMEOUT_SECONDS),
                ),
            ],
        ),
        (
            "PostToolUse",
            vec![group(
                "PushNotification",
                enqueue_handler(cmux_bin, "push-notification", DECLARED_TIMEOUT_SECONDS),
            )],
        ),
        (
            "PermissionRequest",
            vec![group(
                "",
                verb_handler(
                    cmux_bin,
                    &["hooks", "feed", "--source", "claude"],
                    FEED_TIMEOUT_SECONDS,
                    false,
                    false,
                ),
            )],
        ),
    ];

    let hooks_table: serde_json::Map<String, serde_json::Value> = hooks
        .into_iter()
        .map(|(event, groups)| (event.to_owned(), serde_json::Value::Array(groups)))
        .collect();

    serde_json::json!({
        "hooks": serde_json::Value::Object(hooks_table),
        "preferredNotifChannel": "notifications_disabled",
    })
}

/// The document as the wire sees it: compact JSON, keys sorted, slashes
/// unescaped — byte-comparable with upstream's `cliWriteStdout(data)`.
pub fn settings_doc_string(cmux_bin: &Path) -> String {
    serde_json::to_string(&settings_doc_json(cmux_bin)).expect("settings doc serializes")
}

/// The sha256 digest of `doc`, first 8 hex chars — the content-addressed
/// file name stem (upstream: `~/.cmuxterm/claude-settings/<sha256-prefix>.json`).
pub fn doc_hash8(doc: &str) -> String {
    let digest = Sha256::digest(doc.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..8].to_owned()
}

/// Writes the content-addressed settings file under `base_dir` and returns
/// its path. The document is deterministic for a given CLI path, so
/// launches converge on one file (upstream naming, hash prefix as stem).
pub fn write_settings_file(cmux_bin: &Path, base_dir: &Path) -> std::io::Result<PathBuf> {
    let doc = settings_doc_string(cmux_bin);
    std::fs::create_dir_all(base_dir)?;
    let path = base_dir.join(format!("cmux-claude-{}.json", doc_hash8(&doc)));
    std::fs::write(&path, doc)?;
    Ok(path)
}

/// `true` when a spawn program is the claude CLI (`claude`, `claude.exe`,
/// `claude.cmd`) regardless of directory or case.
pub fn is_claude_program(program: &str) -> bool {
    Path::new(program)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.eq_ignore_ascii_case("claude"))
        .unwrap_or(false)
}

/// The claude state dir root: `%LOCALAPPDATA%\cmux\claude-settings`
/// (cmux's own state area, never the agent's `~/.claude`), falling back
/// under the profile dir. Shared by the app's spawn transform and the
/// shim's `launch` verb so both converge on the same content-addressed
/// docs.
pub fn settings_dir() -> Option<PathBuf> {
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(|v| v.to_string_lossy().into_owned());
    let profile = std::env::var_os("USERPROFILE").map(|v| v.to_string_lossy().into_owned());
    let base = match local_app_data.filter(|v| !v.trim().is_empty()) {
        Some(base) => PathBuf::from(base),
        None => PathBuf::from(profile?.trim().to_owned()),
    };
    Some(base.join("cmux").join("claude-settings"))
}

/// The PATH shim directory (one shared dir, not per-surface — the port's
/// deterministic pane ids need no uuid lifetime management; divergence
/// documented in ADR-0011 amendment 4).
pub fn shim_dir(base_dir: &Path) -> PathBuf {
    base_dir.join("shims")
}

/// Writes the `claude.cmd` PATH shim delegating to `hooks claude launch`
/// with the raw user args (`%*`), and returns the shim path. CRLF endings —
/// `cmd.exe` batch labels are unreliable with LF-only files.
pub fn install_shim(shim_dir: &Path, cmux_bin: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(shim_dir)?;
    let path = shim_dir.join("claude.cmd");
    let text = format!(
        "@echo off\r\nrem cmux claude shim (ADR-0011 amendment 4): delegates to cmux.exe hooks claude launch\r\n\"{}\" hooks claude launch %*\r\n",
        cmux_bin.display()
    );
    std::fs::write(&path, text)?;
    Ok(path)
}

/// The pane PATH with the shim directory prepended (Windows PATH entries
/// need no quoting), idempotent when the shim is already first (upstream
/// re-enters its own env the same way).
pub fn path_with_shim(existing: Option<&str>, shim_dir: &Path) -> String {
    let shim = shim_dir.to_string_lossy().into_owned();
    match existing {
        Some(path) if path.split(';').next() == Some(shim.as_str()) => path.to_owned(),
        Some(path) => format!("{shim};{path}"),
        None => shim,
    }
}

/// Resolves the *real* claude along `path_value`, skipping `skip` dirs
/// (the shim dir must be skipped or the launcher calls itself forever).
/// The scan mimics Windows name resolution: per directory, the PATHEXT
/// order used for `claude` (`.exe`, `.bat`, `.cmd`), first hit wins.
pub fn resolve_real_claude(path_value: &str, skip: &[PathBuf]) -> Option<PathBuf> {
    let extensions = [".exe", ".bat", ".cmd"];
    for dir_text in path_value.split(';').filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir_text);
        if skip.iter().any(|skipped| skipped == &dir) {
            continue;
        }
        for extension in extensions {
            let candidate = dir.join(format!("claude{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin() -> &'static Path {
        Path::new(r"C:\build\target\debug\cmux.exe")
    }

    fn groups<'a>(
        hooks: &'a serde_json::Map<String, serde_json::Value>,
        event: &str,
    ) -> &'a Vec<serde_json::Value> {
        hooks.get(event).expect(event).as_array().expect("groups")
    }

    fn handler(group: &serde_json::Value) -> &serde_json::Value {
        &group["hooks"].as_array().unwrap()[0]
    }

    fn assert_matcher(group: &serde_json::Value, expected: &str) {
        assert_eq!(group["matcher"].as_str(), Some(expected));
    }

    /// The golden event table — pins the port to upstream's exact shape
    /// (events, matchers, timeouts, async flags, subcommands).
    #[test]
    fn doc_matches_the_upstream_event_table() {
        let doc = settings_doc_json(bin());
        assert_eq!(
            doc.get("preferredNotifChannel").and_then(|v| v.as_str()),
            Some("notifications_disabled")
        );
        let hook_defs = doc.get("hooks").expect("hooks table").as_object().unwrap();

        // SessionStart: queued session-start + inbox-wait (async, rewake).
        let start = groups(hook_defs, "SessionStart");
        assert_eq!(start.len(), 2);
        assert_matcher(&start[0], "");
        assert_eq!(
            handler(&start[0])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "session-start"])
        );
        assert_eq!(handler(&start[0])["timeout"].as_i64(), Some(5));
        assert_matcher(&start[1], "");
        assert_eq!(
            handler(&start[1])["args"],
            serde_json::json!(["hooks", "claude", "inbox-wait"])
        );
        assert_eq!(handler(&start[1])["timeout"].as_i64(), Some(86_400));
        assert_eq!(handler(&start[1])["async"].as_bool(), Some(true));
        assert_eq!(handler(&start[1])["asyncRewake"].as_bool(), Some(true));

        // Stop: queued stop, queued feed, async auto-name, inbox-wait.
        let stop = groups(hook_defs, "Stop");
        assert_eq!(stop.len(), 4);
        assert_matcher(&stop[0], "");
        assert_eq!(
            handler(&stop[0])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "stop"])
        );
        assert_matcher(&stop[1], "");
        assert_eq!(
            handler(&stop[1])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "feed"])
        );
        assert_matcher(&stop[2], "");
        assert_eq!(
            handler(&stop[2])["args"],
            serde_json::json!(["hooks", "claude", "auto-name"])
        );
        assert_eq!(handler(&stop[2])["timeout"].as_i64(), Some(120));
        assert_eq!(handler(&stop[2])["async"].as_bool(), Some(true));
        assert!(handler(&stop[2])["asyncRewake"].is_null());
        assert_eq!(stop[3]["hooks"][0]["args"], handler(&start[1])["args"]);

        // StopFailure aliases stop and adds inbox-wait only.
        let failure = groups(hook_defs, "StopFailure");
        assert_eq!(failure.len(), 2);
        assert_matcher(&failure[0], "");
        assert_eq!(
            handler(&failure[0])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "stop"])
        );
        // SessionEnd / Notification: single queued groups.
        assert_matcher(&groups(hook_defs, "SessionEnd")[0], "");
        assert_eq!(
            handler(&groups(hook_defs, "SessionEnd")[0])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "session-end"])
        );
        assert_matcher(&groups(hook_defs, "Notification")[0], "");
        assert_eq!(
            handler(&groups(hook_defs, "Notification")[0])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "notification"])
        );

        // UserPromptSubmit: queued prompt-submit + inbox-drain (timeout 5).
        let submit = groups(hook_defs, "UserPromptSubmit");
        assert_eq!(submit.len(), 2);
        assert_matcher(&submit[0], "");
        assert_eq!(
            handler(&submit[0])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "prompt-submit"])
        );
        assert_matcher(&submit[1], "");
        assert_eq!(
            handler(&submit[1])["args"],
            serde_json::json!(["hooks", "claude", "inbox-drain"])
        );
        assert_eq!(handler(&submit[1])["timeout"].as_i64(), Some(5));

        // SubagentStop: one queued feed group.
        assert_matcher(&groups(hook_defs, "SubagentStop")[0], "");
        assert_eq!(
            handler(&groups(hook_defs, "SubagentStop")[0])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "feed"])
        );

        // PreToolUse: CronCreate guard first, then the queued feed.
        let pre = groups(hook_defs, "PreToolUse");
        assert_eq!(pre.len(), 2);
        assert_matcher(&pre[0], "CronCreate");
        assert_eq!(
            handler(&pre[0])["args"],
            serde_json::json!(["hooks", "claude", "cron-create-guard"])
        );
        assert_matcher(&pre[1], "");
        assert_eq!(
            handler(&pre[1])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "pre-tool-use"])
        );

        // PostToolUse: PushNotification only.
        let post = groups(hook_defs, "PostToolUse");
        assert_eq!(post.len(), 1);
        assert_matcher(&post[0], "PushNotification");
        assert_eq!(
            handler(&post[0])["args"],
            serde_json::json!(["hooks", "enqueue", "claude", "push-notification"])
        );

        // PermissionRequest: the direct feed decision hook, 125 s.
        let permission = groups(hook_defs, "PermissionRequest");
        assert_eq!(permission.len(), 1);
        assert_matcher(&permission[0], "");
        assert_eq!(
            handler(&permission[0])["args"],
            serde_json::json!(["hooks", "feed", "--source", "claude"])
        );
        assert_eq!(handler(&permission[0])["timeout"].as_i64(), Some(125));
    }

    /// Handlers in exec form carry the absolute exe as `command` with the
    /// verb list as `args` — Claude Code spawns it directly (no shell).
    #[test]
    fn handlers_are_exec_form_with_the_absolute_bin() {
        let doc = settings_doc_json(bin());
        let stop = &doc["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(
            stop["command"].as_str(),
            Some(r"C:\build\target\debug\cmux.exe")
        );
        assert_eq!(stop["type"].as_str(), Some("command"));
    }

    /// Compact, sorted, slash-safe: the keys of the hooks table sort
    /// alphabetically and handlers carry no statusMessage upstream lacks.
    #[test]
    fn serialized_doc_is_compact_and_sorted() {
        // Compact, sorted, slash-safe: the doc starts with the hooks table
        // ("hooks" sorts before "preferredNotifChannel"), which starts with
        // the alphabetically first event.
        let string = settings_doc_string(bin());
        assert!(!string.contains('\n'));
        assert!(!string.contains(r"\/"));
        assert!(string.starts_with("{\"hooks\":{\"Notification\""));
        let table_order = [
            r#""PermissionRequest""#,
            r#""PostToolUse""#,
            r#""PreToolUse""#,
            r#""SessionEnd""#,
            r#""SessionStart""#,
            r#""Stop""#,
            r#""StopFailure""#,
            r#""SubagentStop""#,
            r#""UserPromptSubmit""#,
        ];
        let mut last = 0;
        for needle in table_order {
            let at = string.find(needle).expect(needle);
            assert!(at > last, "event out of order: {needle}");
            last = at;
        }
        assert!(
            string.find("Notification").unwrap() < string.find(r#""PermissionRequest""#).unwrap()
        );
    }

    /// The doc is deterministic per exec path, and the hash names the
    /// content-addressed file.
    #[test]
    fn doc_is_deterministic_and_hash8_is_stable_and_short() {
        let a = settings_doc_string(bin());
        let b = settings_doc_string(bin());
        assert_eq!(a, b);
        let hash = doc_hash8(&a);
        assert_eq!(hash.len(), 8);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        // A different binary path changes the doc (commands embed it).
        let other = settings_doc_string(Path::new(r"C:\other\cmux.exe"));
        assert_ne!(a, other);
    }

    #[test]
    fn claude_program_detection_is_stem_and_case_insensitive() {
        assert!(is_claude_program("claude"));
        assert!(is_claude_program(r"C:\Users\noah\.local\bin\claude.exe"));
        assert!(is_claude_program(r"D:\npm\claude.cmd"));
        assert!(is_claude_program("CLAUDE.EXE"));
        assert!(!is_claude_program(r"C:\x\claude-helper.exe"));
        assert!(!is_claude_program("claudecode"));
        assert!(!is_claude_program(""));
        assert!(!is_claude_program(r"C:\x\"));
    }

    /// A scratch dir under the system temp, unique per test invocation.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cmux-hooks-claude-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn touch(dir: &Path, file: &str) -> PathBuf {
        std::fs::File::create(dir.join(file)).expect(file);
        dir.join(file)
    }

    #[test]
    fn shim_install_is_crlf_and_delegates_to_the_launch_verb() {
        let dir = scratch("shim-install");
        let cmux_bin = dir.join("cmux.exe");
        let path = install_shim(&dir.join("shims"), &cmux_bin).expect("install");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("\r\n"), "batch files must keep CRLF");
        assert!(text.contains(&format!("\"{}\"", cmux_bin.display())));
        assert!(text.contains("hooks claude launch %*"));
        // Idempotent: a second install keeps the same path and content.
        let path2 = install_shim(&dir.join("shims"), &cmux_bin).expect("reinstall");
        assert_eq!(path, path2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_with_shim_prepends_and_is_idempotent() {
        let shim = PathBuf::from(r"C:\loc\cmux\claude-settings\shims");
        assert_eq!(
            path_with_shim(Some(r"C:\Windows\system32"), &shim),
            r"C:\loc\cmux\claude-settings\shims;C:\Windows\system32"
        );
        // Already first: untouched (no accumulation on re-spawn).
        let already = format!("{};C:\\Windows\\system32", shim.to_string_lossy());
        assert_eq!(path_with_shim(Some(&already), &shim), already);
        assert_eq!(path_with_shim(None, &shim), shim.to_string_lossy());
    }

    #[test]
    fn real_claude_scan_skips_the_shim_and_orders_extensions() {
        let shim = scratch("scan-shim");
        let a = scratch("scan-a");
        let b = scratch("scan-b");
        // The shim dir is present FIRST on the pane PATH and must be
        // skipped, or the launcher would resolve to itself.
        let _ = touch(&shim, "claude.cmd");
        let path = format!("{};{};{}", shim.display(), a.display(), b.display());
        let skip = vec![shim.clone()];

        // Per dir, .exe beats .bat beats .cmd (Windows PATHEXT order).
        let _ = touch(&a, "claude.bat");
        let _ = touch(&a, "claude.cmd");
        let resolved = resolve_real_claude(&path, &skip).expect("resolved");
        assert_eq!(resolved, a.join("claude.bat"));

        let _ = touch(&b, "claude.exe");
        // Windows resolution is directory-major: dir A's .bat beats the
        // later dir's .exe even though .exe leads PATHEXT within a dir.
        let resolved = resolve_real_claude(&path, &skip).expect("resolved 2");
        assert_eq!(
            resolved,
            a.join("claude.bat"),
            "the earlier directory wins, extensions only rank within it"
        );

        // Drop A's .bat: A still owns the slot via .cmd.
        std::fs::remove_file(a.join("claude.bat")).expect("cleanup a-bat");
        let resolved = resolve_real_claude(&path, &skip).expect("resolved 3");
        assert_eq!(resolved, a.join("claude.cmd"));

        // With A emptied, B's .exe is next in line.
        std::fs::remove_file(a.join("claude.cmd")).expect("cleanup a-cmd");
        let resolved = resolve_real_claude(&path, &skip).expect("resolved 4");
        assert_eq!(resolved, b.join("claude.exe"));

        // Only the shim has claude → nothing resolvable (the launcher
        // must not call itself).
        let only_shim = format!("{};{}", shim.display(), a.display());
        let empty = resolve_real_claude(&only_shim, &skip);
        assert!(empty.is_none());
        for dir in [shim, a, b] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
