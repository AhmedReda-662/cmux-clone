//! cmux config file (`~/.config/cmux/cmux.json`) — the port's equivalent of
//! upstream cmux's JSON config.
//!
//! Shape mirrors upstream's `JSONConfigStore` (verified against
//! `cmux-main/Packages/macOS/CmuxSettings/Stores/JSONConfigStore.swift` and
//! `Stores/CmuxConfigLocation.swift`): the file lives at
//! `<home>/.config/cmux/cmux.json`, JSONC (`// line`, `/* block */`, trailing
//! commas) is tolerated on read, every field has a default (a missing file
//! decodes to defaults; unknown fields are ignored), and keys are camelCase
//! nested objects.
//!
//! Recorded divergences (ADR-0008): saves are canonical pretty JSON (upstream
//! edits the source text so comments survive — deferred), there is no
//! file-watcher hot reload yet, and chord bindings parse structurally but the
//! shell applies single strokes only.
//!
//! Upstream field evidence: `globalFontMagnificationPercent` is an `Int`
//! defaulting to 100 (`CmuxFoundation/GlobalFontMagnification.swift`); the
//! shortcut actions `splitRight`, `splitDown`, `equalizeSplits`,
//! `focusNextPane`, `focusPreviousPane`, `resizePane{Left,Right,Up,Down}`
//! come from `CmuxSettings/Values/ShortcutAction.swift`, and
//! `Values/ShortcutKeymapBinding.swift` fixes the hand-editable binding form:
//! a stroke string, a two-stroke chord array, or `null` to unbind.
//! `panes.gutter`, `panes.resizeStep`, `terminal.shell`, and `closePane` are
//! port additions for this slice.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The JSONC template written when the config file does not exist yet. Kept
/// as raw text on purpose: it is documentation a user reads; parsing goes
/// through the same JSONC path as any other content.
pub const DEFAULT_TEMPLATE: &str = r#"{
  // cmux config — JSONC: `//` comments and trailing commas are allowed.
  // Missing keys fall back to their defaults; unknown keys are ignored.

  "appearance": {
    // Magnification percent for the terminal (and any UI) font.
    // Range 50–300 when applied.
    "fontMagnificationPercent": 100
  },

  "panes": {
    // Gutter in pixels between neighboring panes.
    "gutter": 12.0,
    // Pixels per step for keyboard pane resizing.
    "resizeStep": 40
  },

  "terminal": {
    // Custom shell for new panes, e.g.
    // { "program": "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
    //   "args": ["-NoLogo"] }
    // Omit (or leave null) to use the Windows default shell.
    "shell": null
  },

  "theme": {
    // Terminal scheme: "dark" or "light" (null keeps dark). Window
    // chrome and gutters follow `background`; the cells follow the
    // scheme's built-in dark/light colors.
    "scheme": null,
    // Window chrome background as #rrggbb (pane area + gutters).
    "background": null,
    // Terminal cursor color as #rrggbb (when the program has not
    // asked for its own).
    "cursor": null
  },

  "notifications": {
    // Skip the toast while cmux is the focused window (upstream
    // notifications.suppressWhenAppFocused; the ring still updates).
    "suppressWhenAppFocused": false,
    // Play the notification sound even when the notifying pane is
    // already focused (upstream notifications.soundWhenFocused).
    "soundWhenFocused": false
  },

  "shortcuts": {
    // `bindings`: action id → shortcut. A string like "ctrl+shift+r",
    // a two-element array for a chord, or null to unbind the action.
    // Tokens: ctrl, shift, alt, win/cmd (see cmux docs), plus a key such
    // as a letter, digit, tab, or arrow. Not applied (yet): chords.
    "bindings": {
      "splitRight": "ctrl+shift+r",
      "splitDown": "ctrl+shift+d",
      "closePane": "ctrl+shift+w",
      "focusNextPane": "ctrl+tab",
      "focusPreviousPane": "ctrl+shift+tab"
    }
  }
}"#;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    /// Magnification percent, mirroring upstream's
    /// `globalFontMagnificationPercent` (`Int`, default 100). Only clamped
    /// where applied (50–300), not when decoded.
    #[serde(rename = "fontMagnificationPercent")]
    pub font_magnification_percent: i32,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            font_magnification_percent: 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanesConfig {
    /// Gutter in pixels between neighboring panes (each pane inset takes
    /// half on sides that touch a neighbor).
    pub gutter: f64,
    /// Pixels per keyboard pane-resize step. Port value; upstream's dock
    /// resize step was not found during the evidence pass (ADR-0008).
    #[serde(rename = "resizeStep")]
    pub resize_step: u16,
}

impl Default for PanesConfig {
    fn default() -> Self {
        Self {
            gutter: 12.0,
            resize_step: 40,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellCommand {
    pub program: String,
    /// Extra arguments passed to the program. Written with an empty array
    /// when absent so saved config files stay explicit.
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TerminalConfig {
    /// Custom shell for new panes; `None` uses the Windows default shell.
    pub shell: Option<ShellCommand>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ShortcutsConfig {
    /// `shortcuts.bindings`: action id → shortcut override. An empty map
    /// keeps the shell's built-in defaults untouched.
    pub bindings: BTreeMap<String, Binding>,
}

/// `theme`: the port's M2 theme subset. Upstream drives terminal colors
/// from Ghostty config files (`GhosttyConfig*`) and an `AppearanceMode`
/// setting; until that lands (feature row 23, post-MVP) the port takes an
/// explicit scheme plus chrome colors here. Terminal cell colors follow
/// the engine's built-in dark/light schemes — arbitrary hex terminal
/// colors need the Ghostty-config subsystem (or an egui_tty custom-scheme
/// API, which 0.2.0 does not expose).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ThemeConfig {
    /// Terminal scheme: `"dark"` or `"light"` (upstream `AppearanceMode`
    /// case set minus `"system"`, which the port defers). An unknown
    /// value keeps the default scheme at the application site.
    pub scheme: Option<String>,
    /// Window chrome background (`"#rrggbb"`): the pane-area fill, the
    /// gutters, and the base of the rounded pane borders. `null` keeps
    /// the built-in dark chrome.
    pub background: Option<String>,
    /// Terminal cursor color (`"#rrggbb"`) for when the running program
    /// has not asked for its own. `null` follows the selection stroke.
    pub cursor: Option<String>,
}

/// `notifications`: the M3.2b subset. Upstream keeps these as two
/// `notifications.*` settings (`NotificationsCatalogSection`), both
/// defaulting to `false` — verified upstream defaults — so the port uses
/// them directly as config keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NotificationsConfig {
    /// Skip the desktop banner while cmux is the focused window (the
    /// banner is the only effect it removes; sound/flash/command stay).
    pub suppress_when_app_focused: Option<bool>,
    /// Play the notification sound even when the notifying pane is
    /// already focused (off by default upstream: the focused pane shows
    /// its ring).
    pub sound_when_focused: Option<bool>,
}

/// One `commands` entry, mirroring upstream's `CmuxCommandDefinition`
/// (`CmuxConfig.swift` 1524). Upstream's field set: `name` (required,
/// non-blank), `description`, `keywords`, `restart`, `command`,
/// `confirm`, and `workspace` — where `workspace` and `command` are
/// mutually exclusive and one of them is required. `restart` belongs to
/// workspace launches (`CmuxRestartBehavior`: new/recreate/ignore/
/// confirm) and is inert for terminal commands; `workspace` is decoded
/// as opaque JSON because the port has no workspace-definition config
/// surface yet (running one is a recorded slice-1 residual).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CommandDefinition {
    pub name: String,
    pub description: Option<String>,
    /// Palette search keywords upstream (inert until the port grows a
    /// palette; decoded for upstream-file compatibility).
    pub keywords: Vec<String>,
    pub restart: Option<String>,
    /// The text typed into the terminal plus a newline when the command
    /// runs (`CmuxConfigExecutor.prepareShellInputIfAuthorized`).
    pub command: Option<String>,
    /// `true` requires an explicit acknowledgement at run time. Upstream
    /// shows a confirm dialog; the port's runs refuse without the
    /// caller's `ack` (a recorded slice-1 divergence).
    pub confirm: bool,
    pub workspace: Option<serde_json::Value>,
}

impl CommandDefinition {
    /// The decode-time checks upstream performs while decoding
    /// (`CmuxCommandDefinition.init(from:)`, `CmuxConfig.swift`
    /// 1555-1599): a non-blank name, a non-blank `command` when
    /// present, and exactly one of `workspace`/`command`. The messages
    /// mirror upstream's decode-error text so a broken entry fails with
    /// the same reader-visible wording.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Command name must not be blank".to_owned());
        }
        if let Some(command) = &self.command {
            if command.trim().is_empty() {
                return Err(format!(
                    "Command '{}' must not define a blank 'command'",
                    self.name
                ));
            }
        }
        match (&self.workspace, &self.command) {
            (Some(_), Some(_)) => Err(format!(
                "Command '{}' must not define both 'workspace' and 'command'",
                self.name
            )),
            (None, None) => Err(format!(
                "Command '{}' must define either 'workspace' or 'command'",
                self.name
            )),
            _ => Ok(()),
        }
    }

    /// Whether this entry launches a workspace rather than typing a
    /// terminal command.
    pub fn is_workspace_flavor(&self) -> bool {
        self.workspace.is_some()
    }
}

/// Strips the invisible-character set upstream strips before a command
/// reaches a terminal (`CmuxConfigExecutor.sanitizeForDisplay`): zero-
/// width joiners/markers, the bidi-override run, and the zero-width
/// no-break space must not survive into what runs, or a confirm
/// surface could show different text than the shell receives. Ends
/// trimmed like upstream.
pub fn sanitize_command_text(text: &str) -> String {
    const DANGEROUS: &[u32] = &[
        0x200B, 0x200C, 0x200D, 0x200E, 0x200F, 0x202A, 0x202B, 0x202C, 0x202D, 0x202E, 0x2066,
        0x2067, 0x2068, 0x2069, 0xFEFF,
    ];
    text.chars()
        .filter(|c| !DANGEROUS.contains(&(*c as u32)))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The root config model. Unknown fields are ignored for forward
/// compatibility, matching upstream's decode-with-defaults behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    #[serde(rename = "appearance")]
    pub appearance: AppearanceConfig,
    #[serde(rename = "panes")]
    pub panes: PanesConfig,
    #[serde(rename = "terminal")]
    pub terminal: TerminalConfig,
    #[serde(rename = "theme")]
    pub theme: ThemeConfig,
    #[serde(rename = "shortcuts")]
    pub shortcuts: ShortcutsConfig,
    #[serde(rename = "notifications")]
    pub notifications: NotificationsConfig,
    /// `commands`: the upstream custom-command entries
    /// (`CmuxCommandDefinition`). Empty by default; validated at parse
    /// time (blank names and flavor-XOR violations fail the whole
    /// config read exactly like upstream's decode error, so a broken
    /// entry falls back to defaults instead of running half a set).
    #[serde(rename = "commands")]
    pub commands: Vec<CommandDefinition>,
}

/// One `shortcuts.bindings` value in its hand-editable form, mirroring
/// upstream's `ShortcutKeymapBinding`: `"ctrl+shift+r"`, `["ctrl+b", "c"]`,
/// or `null`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// A single stroke.
    Stroke(String),
    /// A prefix stroke followed by a second stroke.
    Chord([String; 2]),
    /// An explicit unbind (JSON `null`).
    Unbound,
}

impl Serialize for Binding {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Binding::Stroke(stroke) => serializer.serialize_str(stroke),
            // `[String; 2]` was validated on decode, but a 2-tuple still
            // serializes as a flat sequence in JSON, matching upstream.
            Binding::Chord(strokes) => serializer.collect_seq(strokes.iter()),
            Binding::Unbound => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for Binding {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Binding;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(
                    "a shortcut string (e.g. \"ctrl+shift+r\"), a two-stroke chord array, or \
                     null to unbind",
                )
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Binding, E> {
                Ok(Binding::Stroke(value.to_owned()))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Binding, A::Error> {
                let mut strokes: Vec<String> = Vec::new();
                while let Some(stroke) = seq.next_element::<String>()? {
                    strokes.push(stroke);
                }
                match strokes.len() {
                    1 => Ok(Binding::Stroke(strokes.remove(0))),
                    2 => Ok(Binding::Chord([strokes.remove(0), strokes.remove(0)])),
                    other => Err(serde::de::Error::custom(format!(
                        "a chord must have exactly two strokes, got {other}"
                    ))),
                }
            }

            // JSON `null` may arrive through either visit hook depending on
            // how the deserializer routes it; both mean an explicit unbind.
            fn visit_none<E: serde::de::Error>(self) -> Result<Binding, E> {
                Ok(Binding::Unbound)
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Binding, E> {
                Ok(Binding::Unbound)
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// Where the config file lives by default: `<home>/.config/cmux/cmux.json`
/// anchored at `%USERPROFILE%`, exactly mirroring upstream's
/// `CmuxConfigLocation`. The shells may override the path through
/// `CMUX_CONFIG_FILE`; `None` when no home directory is set.
pub fn user_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("cmux")
            .join("cmux.json"),
    )
}

impl Config {
    /// Decodes config text, tolerating upstream's JSONC superset on read.
    /// A valid JSON document with an invalid command entry fails here too,
    /// mirroring upstream where the entry's decode error aborts the read.
    pub fn parse(text: &str) -> Result<Self, String> {
        let sanitized = strip_trailing_commas(&strip_comments(text));
        let config: Config =
            serde_json::from_str(&sanitized).map_err(|error| format!("invalid config: {error}"))?;
        for entry in &config.commands {
            if let Err(message) = entry.validate() {
                return Err(format!("invalid config: {message}"));
            }
        }
        Ok(config)
    }

    /// Reads the config file. A missing file is not an error — upstream's
    /// store decodes a missing file (ormalformed value) to the default — so
    /// this returns `Config::default()`.
    pub fn load_file(path: &Path) -> Result<Self, ConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_owned(),
                    source,
                })
            }
        };
        Self::parse(&text).map_err(|message| ConfigError::Parse {
            path: path.to_owned(),
            message,
        })
    }

    /// Encodes and writes the config file, creating missing parent
    /// directories (upstream writes create the file and any missing
    /// directories). Saves are canonical pretty JSON; comments in a
    /// previously hand-edited file are not preserved yet (ADR-0008).
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let body = serde_json::to_string_pretty(self)
            .map_err(|error| ConfigError::Encode { source: error })?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                    path: path.to_owned(),
                    source,
                })?;
            }
        }
        // Upstream publishes atomically (temp + rename). On Windows a rename
        // over an existing file can fail, so fall back to copy-then-clean.
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, body.as_bytes()).map_err(|source| ConfigError::Write {
            path: path.to_owned(),
            source,
        })?;
        match fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(_) => {
                let copied = fs::copy(&temporary, path).is_ok();
                let _ = fs::remove_file(&temporary);
                if copied {
                    Ok(())
                } else {
                    Err(ConfigError::Write {
                        path: path.to_owned(),
                        source: io::Error::other("could not replace the config file"),
                    })
                }
            }
        }
    }
}

/// Errors decoding or persisting the config file. Messages carry the file
/// path so the caller can point the user at the culprit.
#[derive(Debug)]
pub enum ConfigError {
    /// Reading the file failed for a reason other than "not there".
    Read { path: PathBuf, source: io::Error },
    /// The file was read but did not decode into the config model.
    Parse { path: PathBuf, message: String },
    /// Persisting the file failed.
    Write { path: PathBuf, source: io::Error },
    /// Encoding the model to JSON failed.
    Encode { source: serde_json::Error },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Read { path, source } => {
                write!(
                    formatter,
                    "failed to read config {}: {source}",
                    path.display()
                )
            }
            ConfigError::Parse { path, message } => {
                write!(
                    formatter,
                    "failed to decode config {}: {message}",
                    path.display()
                )
            }
            ConfigError::Write { path, source } => {
                write!(
                    formatter,
                    "failed to write config {}: {source}",
                    path.display()
                )
            }
            ConfigError::Encode { source } => {
                write!(formatter, "failed to encode config: {source}")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Read { source, .. } => Some(source),
            ConfigError::Write { source, .. } => Some(source),
            ConfigError::Encode { source } => Some(source),
            ConfigError::Parse { .. } => None,
        }
    }
}

/// Strips upstream's JSONC superset: `// line` and `/* block */` comments.
/// String literals are respected (a `//` inside a string survives); line
/// comment newlines are kept so error line numbers still make sense.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    for rest in chars.by_ref() {
                        if rest == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            None => break,
                            Some('*') if chars.peek() == Some(&'/') => {
                                chars.next();
                                break;
                            }
                            _ => {}
                        }
                    }
                    continue;
                }
                _ => {
                    out.push(c);
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

/// Removes commas followed (outside strings) only by whitespace and then a
/// closing `}` or `]` — upstream tolerates trailing commas on read. Runs
/// after comment stripping so a trailing comma ahead of a comment pair is
/// still recognized.
fn strip_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            let mut lookahead = chars.clone();
            let next_significant = lookahead
                .find(|c| !c.is_whitespace())
                .filter(|c| *c == '}' || *c == ']');
            if next_significant.is_some() {
                continue;
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_and_trailing_commas() {
        let text = r#"{
            // line comment
            "appearance": {
                /* block comment */
                "fontMagnificationPercent": 150,
            },
            "panes": { "gutter": 24.0, },
        }"#;
        let config = Config::parse(text).unwrap();
        assert_eq!(config.appearance.font_magnification_percent, 150);
        assert_eq!(config.panes.gutter, 24.0);
    }

    #[test]
    fn comment_like_text_inside_strings_survives() {
        let text = r#"{ "terminal": { "shell": {
            "program": "C://path//program", "args": ["/* not a comment */"],
        } } }"#;
        let config = Config::parse(text).unwrap();
        let shell = config.terminal.shell.unwrap();
        assert_eq!(shell.program, "C://path//program");
        assert_eq!(shell.args, vec!["/* not a comment */"]);
    }

    #[test]
    fn escaped_quotes_inside_strings_survive() {
        let text = r#"{ "terminal": { "shell":
            { "program": "cmd /c \"echo hello // world\"" } } }"#;
        let config = Config::parse(text).unwrap();
        assert_eq!(
            config.terminal.shell.unwrap().program,
            r#"cmd /c "echo hello // world""#
        );
    }

    #[test]
    fn missing_file_decodes_to_defaults() {
        let path = std::env::temp_dir().join(format!(
            "cmux-config-test-{}-missing.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let config = Config::load_file(&path).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn full_user_config_decodes_all_sections() {
        let text = r#"{
            "appearance": { "fontMagnificationPercent": 175 },
            "panes": { "gutter": 30.0, "resizeStep": 64 },
            "terminal": { "shell": { "program": "pwsh.exe", "args": ["-NoLogo"] } },
            "shortcuts": { "bindings": {
                "splitRight": "ctrl+shift+s",
                "closePane": null,
                "someChord": ["ctrl+b", "d"]
            } }
        }"#;
        let config = Config::parse(text).unwrap();
        assert_eq!(config.appearance.font_magnification_percent, 175);
        assert_eq!(config.panes.gutter, 30.0);
        assert_eq!(config.panes.resize_step, 64);
        assert_eq!(config.terminal.shell.as_ref().unwrap().program, "pwsh.exe");
        assert_eq!(
            config.terminal.shell.as_ref().unwrap().args,
            vec!["-NoLogo"]
        );
        assert_eq!(
            config.shortcuts.bindings.get("splitRight").unwrap(),
            &Binding::Stroke("ctrl+shift+s".to_owned())
        );
        assert_eq!(
            config.shortcuts.bindings.get("closePane"),
            Some(&Binding::Unbound)
        );
        assert_eq!(
            config.shortcuts.bindings.get("someChord").unwrap(),
            &Binding::Chord(["ctrl+b".to_owned(), "d".to_owned()])
        );
    }

    #[test]
    fn theme_section_decodes_all_fields_and_defaults() {
        let config = Config::parse(
            r##"{
            "theme": {
                "scheme": "light",
                "background": "#1a1b2e",
                "cursor": "ffcc00"
            }
        }"##,
        )
        .unwrap();
        assert_eq!(config.theme.scheme.as_deref(), Some("light"));
        assert_eq!(config.theme.background.as_deref(), Some("#1a1b2e"));
        assert_eq!(config.theme.cursor.as_deref(), Some("ffcc00"));
        assert_eq!(Config::default().theme, ThemeConfig::default());
    }

    #[test]
    fn template_decodes_and_carries_the_theme_section() {
        let config = Config::parse(DEFAULT_TEMPLATE).unwrap();
        assert!(config.theme.scheme.is_none());
        assert_eq!(config.theme, ThemeConfig::default());
    }

    #[test]
    fn theme_round_trips_through_save() {
        let config =
            Config::parse(r##"{ "theme": { "scheme": "light", "background": "#fdf9ff" } }"##)
                .unwrap();
        let path = std::env::temp_dir().join(format!(
            "cmux-config-test-{}-theme.json",
            std::process::id()
        ));
        config.save(&path).unwrap();
        let loaded = Config::load_file(&path).unwrap();
        assert_eq!(loaded, config);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn invalid_binding_type_is_rejected() {
        let error =
            Config::parse(r#"{ "shortcuts": { "bindings": { "splitRight": 42 } } }"#).unwrap_err();
        assert!(error.contains("shortcut"), "{error}");
    }

    #[test]
    fn chord_must_have_two_strokes() {
        let error = Config::parse(r#"{ "shortcuts": { "bindings": { "x": ["a", "b", "c"] } } }"#)
            .unwrap_err();
        assert!(error.contains("two strokes"), "{error}");
    }

    #[test]
    fn round_trip_preserves_values() {
        let config = Config::parse(
            r#"{ "appearance": { "fontMagnificationPercent": 120 },
                 "panes": { "gutter": 18.5 },
                 "terminal": { "shell": { "program": "pwsh.exe", "args": ["-Logo"] } },
                 "shortcuts": { "bindings": { "splitRight": "ctrl+shift+s" } } }"#,
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "cmux-config-test-{}-roundtrip.json",
            std::process::id()
        ));
        let sub_path = path.join(".config").join("cmux").join("cmux.json");
        config.save(&sub_path).unwrap();
        let loaded = Config::load_file(&sub_path).unwrap();
        assert_eq!(loaded, config);
        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let text = r#"{ "futureSection": { "anything": true },
                         "appearance": { "futureField": 3 } }"#;
        let config = Config::parse(text).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn default_template_parses() {
        let config = Config::parse(DEFAULT_TEMPLATE).unwrap();
        assert_eq!(config.appearance.font_magnification_percent, 100);
        assert_eq!(config.panes.gutter, 12.0);
        assert!(config.terminal.shell.is_none());
        assert_eq!(config.shortcuts.bindings.len(), 5);
        assert_eq!(
            config.shortcuts.bindings.get("closePane"),
            Some(&Binding::Stroke("ctrl+shift+w".to_owned()))
        );
    }

    #[test]
    fn binding_serializes_canonical_forms() {
        assert_eq!(
            serde_json::to_value(Binding::Stroke("ctrl+t".to_owned())).unwrap(),
            "ctrl+t"
        );
        assert_eq!(
            serde_json::to_value(Binding::Chord(["a".to_owned(), "b".to_owned()])).unwrap(),
            serde_json::json!(["a", "b"])
        );
        assert_eq!(
            serde_json::to_value(Binding::Unbound).unwrap(),
            serde_json::Value::Null
        );
    }

    #[test]
    fn commands_default_to_empty_and_round_trip() {
        let config = Config::parse("{}").unwrap();
        assert!(config.commands.is_empty());

        let config = Config::parse(
            r#"{"commands": [
                {"name": "build", "description": "cargo check", "command": "cargo check"},
                {"name": "guarded", "command": "shutdown /r", "confirm": true},
                {"name": "space", "workspace": {"window": {"kind": "terminal"}}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(config.commands.len(), 3);
        assert_eq!(config.commands[0].name, "build");
        assert_eq!(config.commands[0].command.as_deref(), Some("cargo check"));
        assert!(!config.commands[0].confirm);
        assert!(!config.commands[0].is_workspace_flavor());
        assert!(config.commands[1].confirm);
        assert!(config.commands[2].is_workspace_flavor());
        assert!(config.commands[2].workspace.as_ref().unwrap().is_object());

        let text = serde_json::to_string(&config).unwrap();
        let round = Config::parse(&text).unwrap();
        assert_eq!(config, round);
    }

    #[test]
    fn commands_validate_like_upstreams_decode() {
        // Blank name (upstream: "Command name must not be blank").
        let error = Config::parse(r#"{"commands": [{"name": " ", "command": "x"}]}"#).unwrap_err();
        assert!(error.contains("Command name must not be blank"), "{error}");

        // Blank command.
        let error =
            Config::parse(r#"{"commands": [{"name": "a", "command": "   "}]}"#).unwrap_err();
        assert!(
            error.contains("Command 'a' must not define a blank 'command'"),
            "{error}"
        );

        // Both flavors.
        let error =
            Config::parse(r#"{"commands": [{"name": "a", "command": "x", "workspace": {}}]}"#)
                .unwrap_err();
        assert!(
            error.contains("Command 'a' must not define both 'workspace' and 'command'"),
            "{error}"
        );

        // Neither flavor.
        let error = Config::parse(r#"{"commands": [{"name": "a"}]}"#).unwrap_err();
        assert!(
            error.contains("Command 'a' must define either 'workspace' or 'command'"),
            "{error}"
        );
    }

    #[test]
    fn sanitize_command_text_strips_invisible_scalars() {
        // The bidi-override pair and the zero-width no-break space must
        // not reach a terminal; sanitizer output is also trimmed.
        let cleaned = sanitize_command_text("\u{202A}echo hi\u{202C}\u{FEFF}  ");
        assert_eq!(cleaned, "echo hi");

        // Ordinary text passes through untouched (newlines trimmed like
        // upstream; interior whitespace is preserved).
        assert_eq!(
            sanitize_command_text("  cargo check --all-targets  "),
            "cargo check --all-targets"
        );
    }
}
