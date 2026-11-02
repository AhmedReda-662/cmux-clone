//! The port's agent-hook catalog, first entry `codex` — a one-for-one port
//! of upstream's `AgentHookDef` for codex
//! (`CLI/CMUXCLI+AgentHookCatalog.swift:8-28`):
//!
//! ```text
//! name: "codex", displayName: "Codex",
//! configDir: ".codex", configFile: "hooks.json", configDirEnvOverride: "CODEX_HOME",
//! disableEnvVar: "CMUX_CODEX_HOOKS_DISABLED", hookMarker: "cmux hooks codex",
//! format: .nested(timeoutMs: 5_000),
//! events: SessionStart→session-start, UserPromptSubmit→prompt-submit, Stop→stop,
//! feedHookEvents: PreToolUse … SubagentStop (7),
//! postInstallAction: .codexConfigToml, no config-dir creation (default false)
//! ```
//!
//! Slice-A divergences (ADR-0011 amendment 3, documented):
//! - commands are **direct exe paths** (`"<abs>cmux.exe" hooks enqueue codex
//!   <sub>`) instead of upstream's content-addressed `#!/bin/sh` spool
//!   scripts — the Windows process tree has no spool forwarder yet, and an
//!   exe path is the command shape every Windows shell can run;
//! - the **feed** events are not installed yet (the port has no journal
//!   feed bridge; installing no-op hooks would only slow the agent); the
//!   plumbing keeps the event labels so a later slice can add them;
//! - `CMUX_AGENT_HOOK_STATE_DIR`-style routing keys are reduced to the
//!   allow-list in `cmux-ipc::protocol::ROUTED_ENV_KEYS`.

use std::path::{Path, PathBuf};

/// One hook event in a catalog entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookEvent {
    /// The event name the agent's runtime uses (`"Stop"`).
    pub agent_event: &'static str,
    /// The cmux hook subcommand (`"stop"`).
    pub cmux_subcommand: &'static str,
}

/// A static catalog entry (`AgentHookDef` upstream).
pub struct HookDef {
    pub name: &'static str,
    pub display_name: &'static str,
    /// e.g. `".codex"` under the home dir.
    pub config_subdir: &'static str,
    pub config_file: &'static str,
    pub config_dir_env_override: Option<&'static str>,
    pub disable_env_var: &'static str,
    pub hook_marker: &'static str,
    /// Nested-format hooks.json entries: timeout declared in ms, written as
    /// whole seconds for codex (`AgentHookDeliveryPolicy.declaredTimeoutSeconds`).
    pub nested_timeout_ms: u64,
    pub events: &'static [HookEvent],
    /// Feed-bridge event names; slice A keeps the list for data only.
    pub feed_hook_events: &'static [&'static str],
    pub declares_config_toml: bool,
    /// Upstream default `false`: a missing config dir means "agent CLI not
    /// set up"; we do not fabricate dot-directories.
    pub create_config_dir_if_missing: bool,
}

pub const CODEX: HookDef = HookDef {
    name: "codex",
    display_name: "Codex",
    config_subdir: ".codex",
    config_file: "hooks.json",
    config_dir_env_override: Some("CODEX_HOME"),
    disable_env_var: "CMUX_CODEX_HOOKS_DISABLED",
    hook_marker: "cmux hooks codex",
    nested_timeout_ms: 5_000,
    events: &[
        HookEvent {
            agent_event: "SessionStart",
            cmux_subcommand: "session-start",
        },
        HookEvent {
            agent_event: "UserPromptSubmit",
            cmux_subcommand: "prompt-submit",
        },
        HookEvent {
            agent_event: "Stop",
            cmux_subcommand: "stop",
        },
    ],
    feed_hook_events: &[
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "PreCompact",
        "PostCompact",
        "SubagentStart",
        "SubagentStop",
    ],
    declares_config_toml: true,
    create_config_dir_if_missing: false,
};

/// Catalog entry for Claude Code (M3.3b). There is no upstream
/// `AgentHookDef` for claude — upstream special-cases it (no config-file
/// installer; the wrapper injects generated settings per launch). The
/// entry exists for the port's shared machinery: the enqueue client's
/// disable-variable gate and future command-ownership checks. The fields
/// that only a config-file installer would use describe claude's own
/// settings file and are documented as never-written.
pub const CLAUDE: HookDef = HookDef {
    name: "claude",
    display_name: "Claude",
    config_subdir: ".claude",
    // `claude`'s own settings file. Upstream never edits it and neither
    // does the port — the doc generator (`claude::settings_doc_json`)
    // produces per-launch documents the agent gets via `--settings`.
    config_file: "settings.json",
    // Upstream's app forwards an inherited `CLAUDE_CONFIG_DIR` to panes;
    // the port has no claude config-file installer to resolve with yet,
    // so no override is wired.
    config_dir_env_override: None,
    disable_env_var: "CMUX_CLAUDE_HOOKS_DISABLED",
    hook_marker: "cmux hooks claude",
    // Not used: claude's schema takes whole seconds directly and the doc
    // generator carries per-hook timeouts. Kept in step with the shared
    // declared-timeout constant for consistency.
    nested_timeout_ms: 5_000,
    // The six lifecycle events with queued admission groups (upstream
    // `lifecycleDefinitions`; StopFailure aliases `stop`).
    events: &[
        HookEvent {
            agent_event: "SessionStart",
            cmux_subcommand: "session-start",
        },
        HookEvent {
            agent_event: "UserPromptSubmit",
            cmux_subcommand: "prompt-submit",
        },
        HookEvent {
            agent_event: "Stop",
            cmux_subcommand: "stop",
        },
        HookEvent {
            agent_event: "StopFailure",
            cmux_subcommand: "stop",
        },
        HookEvent {
            agent_event: "SessionEnd",
            cmux_subcommand: "session-end",
        },
        HookEvent {
            agent_event: "Notification",
            cmux_subcommand: "notification",
        },
    ],
    // Claude installs its extra route groups (`feed`, `pre-tool-use`,
    // `push-notification`) in the generated settings doc, not as codex
    // style per-event feed installs — nothing to list here.
    feed_hook_events: &[],
    declares_config_toml: false,
    // A missing `~/.claude` means the agent CLI is not installed; the port
    // fabricates nothing (upstream `postInstallAction` guard philosophy).
    create_config_dir_if_missing: false,
};

/// Catalog lookup — currently `codex` (slice A) and `claude` (M3.3b); the
/// set grows with each installer slice
/// (`server::protocol::KNOWN_AGENTS` mirrors it).
pub fn hook_def(name: &str) -> Option<&'static HookDef> {
    match name {
        "codex" => Some(&CODEX),
        "claude" => Some(&CLAUDE),
        _ => None,
    }
}

impl HookDef {
    /// The resolved config dir given an env lookup (pure; testable): the
    /// env-override wins (upstream `resolvedConfigDir`), else the subdir
    /// under the home directory.
    pub fn resolved_config_dir_with(
        &self,
        env_override: Option<&str>,
        user_profile: Option<&str>,
    ) -> Option<PathBuf> {
        if let Some(value) = env_override.map(str::trim).filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(value));
        }
        let home = user_profile?;
        if home.is_empty() {
            return None;
        }
        Some(Path::new(home).join(self.config_subdir))
    }

    /// The resolved config dir using the real process environment.
    pub fn resolved_config_dir(&self) -> Option<PathBuf> {
        let override_value = self
            .config_dir_env_override
            .and_then(|key| std::env::var(key).ok());
        let home = std::env::var_os("USERPROFILE").map(|os| os.to_string_lossy().into_owned());
        self.resolved_config_dir_with(override_value.as_deref(), home.as_deref())
    }

    /// The hooks.json path as cwd-independent as the catalog can make it.
    pub fn hooks_json_path(&self) -> Option<PathBuf> {
        self.resolved_config_dir()
            .map(|dir| dir.join(self.config_file))
    }

    /// Nested-format timeout in whole seconds for codex schemas
    /// (`nestedHookTimeout`: codex/grok/muse write seconds).
    pub fn nested_timeout_seconds(&self) -> i64 {
        ((self.nested_timeout_ms / 1000) as i64).max(1)
    }

    /// The exact command the port installs for one event: a quoted absolute
    /// exe path plus the enqueue args. Direct invocation works under
    /// CreateProcess, `cmd /c`, and POSIX-ish shells alike; the enqueue
    /// client performs the admission gating internally (env check, fail
    /// open on `{}`), which moves upstream's shell guard into the client.
    pub fn hook_command(&self, exe_path: &Path, event: HookEvent) -> String {
        format!(
            "\"{}\" hooks enqueue {} {}",
            exe_path.display(),
            self.name,
            event.cmux_subcommand
        )
    }
}

/// Upstream `isCmuxOwnedHookCommand` for the port (slice A): the exact
/// current command form, any cmux.exe enqueue form (paths change between
/// builds — ownership follows the executable), port-and-upstream script
/// paths under `~/.cmux/hooks`, and the legacy token forms. Non-cmux
/// entries are never matched — pruning removes only what cmux wrote
/// (manifest §9: nothing silently removed).
pub fn is_cmux_owned_hook_command(command: &str, def: &HookDef, include_legacy: bool) -> bool {
    // Upstream script-path ownership analog (Mac stores .sh, the port could
    // store .cmd one day): a file under ~/.cmux/hooks named cmux-codex-*.
    let lower = command.to_ascii_lowercase();
    let is_script_path = (lower.contains(r"\.cmux\hooks\cmux-codex-hook-")
        || lower.contains("/.cmux/hooks/cmux-codex-hook-"))
        && (lower.ends_with(".sh") || lower.ends_with(".cmd"));
    if is_script_path {
        return true;
    }
    // The port's direct-exe form: any cmux.exe enqueueing this agent's
    // events or running that agent's verbs is ours, whatever build it
    // came from (upstream's legacy matcher likewise owns
    // `cmux hooks <agent> …` for every verb).
    if lower.contains(r"\cmux.exe")
        && (lower.contains(&format!("hooks enqueue {}", def.name))
            || lower.contains(&format!("hooks {}", def.name)))
    {
        return true;
    }
    if include_legacy && is_legacy_cmux_owned_hook_command(command, def) {
        return true;
    }
    false
}

/// Legacy-token check, port of upstream `isLegacyCmuxOwnedHookTokens`
/// (`CMUXCLI+AgentHookDefinitions.swift:701-735`): first token's basename
/// must be `cmux` (or `cmux.exe`), then one of the historical command
/// shapes; shell-guard prefixes and the `|| echo '{}'` fallback are
/// stripped first, mirroring `legacyCmuxCommandTokenLists`.
pub fn is_legacy_cmux_owned_hook_command(command: &str, def: &HookDef) -> bool {
    let mut body = command.trim().to_owned();
    for prefix in [
        format!(
            "[ -n \"$CMUX_SURFACE_ID\" ] && [ \"${}\" != \"1\" ] && command -v cmux >/dev/null 2>&1 && ",
            def.disable_env_var
        ),
        format!(
            "[ \"${}\" != \"1\" ] && command -v cmux >/dev/null 2>&1 && ",
            def.disable_env_var
        ),
    ] {
        if let Some(rest) = body.strip_prefix(&prefix) {
            body = rest.to_owned();
            break;
        }
    }
    if let Some(rest) = body.strip_suffix(" || echo '{}'") {
        body = rest.to_owned();
    }
    if body.contains(';') || body.contains('|') || body.contains('&') || body.contains('`') {
        return false;
    }
    let tokens: Vec<&str> = body.split([' ', '\t']).filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return false;
    }
    let basename = tokens[0]
        .trim_matches('"')
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .trim_matches('"')
        .to_ascii_lowercase();
    if basename != "cmux" && basename != "cmux.exe" {
        return false;
    }
    if tokens.len() >= 2 && tokens[1] == "codex-hook" {
        return true;
    }
    if tokens.len() >= 4
        && tokens[1] == "feed-hook"
        && tokens[2] == "--source"
        && tokens[3] == def.name
    {
        return true;
    }
    if tokens.len() >= 5 && tokens[1] == "hooks" && tokens[2] == "enqueue" && tokens[3] == def.name
    {
        return true;
    }
    if tokens.len() >= 3 && tokens[1] == "hooks" && tokens[2] == def.name {
        return true;
    }
    if tokens.len() >= 5
        && tokens[1] == "hooks"
        && tokens[2] == "feed"
        && tokens[3] == "--source"
        && tokens[4] == def.name
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> &'static HookDef {
        hook_def("codex").unwrap()
    }

    #[test]
    fn resolved_config_dir_prefers_the_env_override() {
        let d = def();
        // The override wins even when it is empty-looking enough to be valid.
        assert_eq!(
            d.resolved_config_dir_with(Some(r"C:\tmp\codex-home"), Some(r"C:\home")),
            Some(PathBuf::from(r"C:\tmp\codex-home"))
        );
        // Blank overrides fall back to the home path.
        assert_eq!(
            d.resolved_config_dir_with(Some("   "), Some(r"C:\home")),
            Some(PathBuf::from(r"C:\home\.codex"))
        );
        // No home, no override, no answer.
        assert_eq!(d.resolved_config_dir_with(None, None), None);
        assert_eq!(d.resolved_config_dir_with(None, Some("")), None);
        // The default resolution reads USERPROFILE at runtime.
        std::env::remove_var("CODEX_HOME");
        let home = std::env::var_os("USERPROFILE").expect("USERPROFILE exists");
        let fallback = d.resolved_config_dir().expect("fallback dir");
        assert_eq!(fallback, Path::new(&home).join(".codex"));
    }

    #[test]
    fn owned_commands_match_by_executable_and_enqueue_token() {
        let d = def();
        // The exact current form across paths.
        assert!(is_cmux_owned_hook_command(
            r#""C:\build\target\debug\cmux.exe" hooks enqueue codex stop"#,
            d,
            false
        ));
        assert!(is_cmux_owned_hook_command(
            r#""D:\workspace\Projects\cmux-clone\port\target\release\cmux.exe" hooks enqueue codex session-start"#,
            d,
            false
        ));
        // Lowercase matching works (paths are case-insensitive on Windows).
        assert!(is_cmux_owned_hook_command(
            r#""c:\build\CMUX.EXE" hooks enqueue codex stop"#,
            d,
            false
        ));
        // A user's own command is never claimed.
        assert!(!is_cmux_owned_hook_command(
            "python notify.py --agent-uv codex hook",
            d,
            false
        ));
        assert!(!is_cmux_owned_hook_command(
            r#""C:\other\tools.exe" hooks enqueue codex stop"#,
            d,
            false
        ));
        // Unrelated cmux.exe commands (not enqueueing codex) are not owned.
        assert!(!is_cmux_owned_hook_command(
            r#""C:\build\cmux.exe" status"#,
            d,
            false
        ));
        // Script paths both POSIX and Windows styled.
        assert!(is_cmux_owned_hook_command(
            r"C:\Users\noah\.cmux\hooks\cmux-codex-hook-stop.sh",
            d,
            false
        ));
        assert!(is_cmux_owned_hook_command(
            "/Users/x/.cmux/hooks/cmux-codex-hook-ab12cdef-9-stop.sh",
            d,
            false
        ));
    }

    #[test]
    fn legacy_tokens_are_owned_and_guards_are_stripped() {
        let d = def();
        assert!(is_cmux_owned_hook_command("cmux codex-hook stop", d, true));
        assert!(is_cmux_owned_hook_command(
            "cmux hooks enqueue codex stop",
            d,
            true
        ));
        assert!(is_cmux_owned_hook_command(
            "cmux.exe hooks codex stop",
            d,
            true
        ));
        assert!(is_cmux_owned_hook_command(
            r#""C:\x\cmux.exe" hooks feed --source codex stop"#,
            d,
            true
        ));
        assert!(is_cmux_owned_hook_command(
            r#""C:\x\cmux.exe" feed-hook --source codex stop"#,
            d,
            true
        ));
        // Guarded legacy form (macOS-era entries): the guard is stripped.
        assert!(is_cmux_owned_hook_command(
            r#"[ -n "$CMUX_SURFACE_ID" ] && [ "$CMUX_CODEX_HOOKS_DISABLED" != "1" ] && command -v cmux >/dev/null 2>&1 && "C:\x\cmux.exe" hooks enqueue codex stop || echo '{}'"#,
            d,
            true
        ));
        // Without legacy matching, guarded sh forms are not owned.
        assert!(!is_cmux_owned_hook_command(
            "cmux codex-hook stop",
            d,
            false
        ));
        // Unrelated tokens are never owned.
        assert!(!is_cmux_owned_hook_command(
            "codex hooks codex stop",
            d,
            true
        ));
        assert!(!is_cmux_owned_hook_command(
            "cmux hooks enqueue OTHER stop",
            d,
            true
        ));
    }

    #[test]
    fn nested_timeout_is_seconds() {
        assert_eq!(def().nested_timeout_seconds(), 5);
    }

    #[test]
    fn catalog_has_codex_first_and_claude_second() {
        assert_eq!(hook_def("codex").map(|d| d.display_name), Some("Codex"));
        assert_eq!(
            hook_def("claude").map(|d| d.disable_env_var),
            Some("CMUX_CLAUDE_HOOKS_DISABLED")
        );
        // The catalog stops there for now; unknown agents stay unknown.
        assert!(hook_def("grok").is_none());
    }

    #[test]
    fn claude_queued_commands_are_owned_by_the_claude_def() {
        let d = hook_def("claude").unwrap();
        assert!(is_cmux_owned_hook_command(
            r#""C:\build\target\debug\cmux.exe" hooks enqueue claude stop"#,
            d,
            false
        ));
        assert!(is_cmux_owned_hook_command(
            r#""C:\build\target\debug\cmux.exe" hooks claude auto-name"#,
            d,
            false
        ));
        // A plain claude.exe invocation is not a cmux-owned hook.
        assert!(!is_cmux_owned_hook_command(
            r"C:\Users\noah\.local\bin\claude.exe",
            d,
            false
        ));
        // Another agent's enqueue shape does not match claude's def.
        assert!(!is_cmux_owned_hook_command(
            r#""C:\build\target\debug\cmux.exe" hooks enqueue codex stop"#,
            d,
            false
        ));
    }
}
