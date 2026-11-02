//! cmux-hooks — agent-hook config installers for the port (M3.3 slice A).
//!
//! One-for-one port of upstream's codex hooks integration
//! (`cmux hooks codex install|uninstall`): the `.codex/hooks.json`
//! prune-cmux-only merge, the `config.toml` marker-block editor
//! (feature flag + hook-trust tables), and the hook-command ownership
//! rules — with the slice-A divergences documented in
//! `catalog`/`ADR-0011` (direct-exe hook commands instead of sh spool
//! scripts; feeds not installed yet).
//!
//! Every mutation of the agent's configuration happens only through
//! explicit install/uninstall calls that are handed a resolved path —
//! nothing writes to an agent's home dir implicitly.

pub mod catalog;
pub mod claude;
pub mod config_toml;
pub mod hash;
pub mod hooks_json;

pub use catalog::{hook_def, HookDef, HookEvent, CLAUDE, CODEX};
pub use config_toml::{installing_hooks, uninstalling_hooks, InstallResult, TrustEntry};
pub use hooks_json::{desired_group, desired_hooks, MergeOutcome};
pub use hooks_json::{install as install_hooks_json, uninstall as uninstall_hooks_json};

use std::path::{Path, PathBuf};

/// Event labels Codex uses for trust keys (upstream `codexHookEventLabel`),
/// all events upstream iterates (`codexHookEventNames`), in list order.
pub const TRUST_EVENT_LABELS: [(&str, &str); 10] = [
    ("PreToolUse", "pre_tool_use"),
    ("PermissionRequest", "permission_request"),
    ("PostToolUse", "post_tool_use"),
    ("PreCompact", "pre_compact"),
    ("PostCompact", "post_compact"),
    ("SessionStart", "session_start"),
    ("SubagentStart", "subagent_start"),
    ("SubagentStop", "subagent_stop"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("Stop", "stop"),
];

/// Events whose group supports `matcher` (upstream
/// `codexHookEventUsesMatcher`).
pub const USES_MATCHER: [&str; 8] = [
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "SessionStart",
    "SubagentStart",
    "SubagentStop",
];

pub fn uses_matcher(event: &str) -> bool {
    USES_MATCHER.contains(&event)
}

/// Trust keys use the realpath of the hooks file when the agent resolves
/// its config dir from an env override (upstream
/// `codexHookTrustKeySourcePath`); the port keeps the constructed path
/// (canonicalizing when the directory exists so the key is stable across
/// relative/absolute spellings).
pub fn trust_key_source_path(hooks_json_path: &Path) -> PathBuf {
    if let (Some(parent), Some(file)) = (hooks_json_path.parent(), hooks_json_path.file_name()) {
        if let Ok(resolved) = std::fs::canonicalize(parent) {
            return verified_path(resolved.join(file));
        }
    }
    verified_path(hooks_json_path.to_path_buf())
}

/// Strips Windows verbatim prefixes so keys spell the path the user sees.
fn verified_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{}", stripped));
    }
    if let Some(stripped) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    path
}

/// Computes the trust entries for the cmux-owned commands in `hooks`
/// (upstream `codexHookTrustEntries` with `includeLegacyOwnedCommands`
/// defaulting to `false` for install, `true` for uninstall).
pub fn codex_trust_entries(
    hooks: &serde_json::Value,
    def: &HookDef,
    hooks_json_path: &Path,
    include_legacy: bool,
) -> Vec<TrustEntry> {
    let key_source = trust_key_source_path(hooks_json_path);
    let mut entries = Vec::new();
    let Some(hooks_obj) = hooks.get("hooks").and_then(serde_json::Value::as_object) else {
        return entries;
    };
    for (event_name, label) in TRUST_EVENT_LABELS {
        let Some(groups) = hooks_obj
            .get(event_name)
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for (group_index, group) in groups.iter().enumerate() {
            let Some(hook_list) = group.get("hooks").and_then(serde_json::Value::as_array) else {
                continue;
            };
            let matcher = if uses_matcher(event_name) {
                group.get("matcher").and_then(serde_json::Value::as_str)
            } else {
                None
            };
            for (handler_index, hook) in hook_list.iter().enumerate() {
                let Some(command) = hook.get("command").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                if !catalog::is_cmux_owned_hook_command(command, def, include_legacy) {
                    continue;
                }
                let timeout = hook
                    .get("timeout")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(600)
                    .max(1);
                let status_message = hook
                    .get("statusMessage")
                    .and_then(serde_json::Value::as_str);
                let key = format!(
                    "{}:{}:{}:{}",
                    key_source.display(),
                    label,
                    group_index,
                    handler_index
                );
                let trusted_hash =
                    hash::codex_command_hook_hash(label, matcher, command, timeout, status_message);
                entries.push(TrustEntry { key, trusted_hash });
            }
        }
    }
    entries
}

/// The key prefixes whose stale trust tables may be cleared on reinstall
/// (upstream `codexHookTrustKeyPrefixes` — resolved and normalized paths).
pub fn codex_trust_key_prefixes(hooks_json_path: &Path) -> Vec<String> {
    vec![format!(
        "{}:",
        trust_key_source_path(hooks_json_path).display()
    )]
}

/// What `cmux hooks codex install` should do, pre-execution (tests assert
/// on this; the CLI displays it).
#[derive(Debug, Clone, PartialEq)]
pub enum CodexInstallPlan {
    /// Write this content to hooks.json and trust to config.toml.
    Write {
        hooks_json_path: PathBuf,
        config_toml_path: PathBuf,
        hooks_content: String,
        config_content: String,
        /// When config.toml did not exist, the editor's output is not
        /// written (upstream `postInstallAction` runs only when the file
        /// exists — cmux never fabricates agent config files).
        config_toml_existed: bool,
    },
    /// Everything already in place; skip the writes.
    UpToDate,
}

/// Error cases in the installer contract (upstream messages kept).
#[derive(Debug, PartialEq)]
pub enum CodexInstallError {
    /// `<file> exists but is not valid JSON. Fix or remove it before installing hooks.`
    InvalidHooksJson(String),
    /// The config directory cannot be created or the agent config is absent.
    ConfigMissing(String),
    Io(String),
}

impl std::fmt::Display for CodexInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodexInstallError::InvalidHooksJson(message) => write!(f, "{message}"),
            CodexInstallError::ConfigMissing(message) => write!(f, "{message}"),
            CodexInstallError::Io(message) => write!(f, "I/O error: {message}"),
        }
    }
}

/// The codex installer: resolved paths + ownership context.
pub struct CodexHooks {
    pub def: &'static HookDef,
    pub config_dir: PathBuf,
    pub exe_path: PathBuf,
}

impl CodexHooks {
    /// Resolves `~/.codex` (or `CODEX_HOME`) for the codex def.
    pub fn resolve(exe_path: Option<PathBuf>) -> Option<Self> {
        let def = hook_def("codex")?;
        let config_dir = def.resolved_config_dir()?;
        Some(CodexHooks::with_config_dir(def, config_dir, exe_path))
    }

    /// Explicit-path constructor (used by tests and future plumbing that
    /// resolves the directory itself).
    pub fn with_config_dir(
        def: &'static HookDef,
        config_dir: PathBuf,
        exe_path: Option<PathBuf>,
    ) -> Self {
        CodexHooks {
            def,
            config_dir,
            exe_path: exe_path.unwrap_or_else(|| std::env::current_exe().unwrap_or_default()),
        }
    }

    pub fn hooks_json_path(&self) -> PathBuf {
        self.config_dir.join(self.def.config_file)
    }

    pub fn config_toml_path(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// What this exe would install for a set of subcommands.
    pub fn desired_hooks(&self) -> serde_json::Map<String, serde_json::Value> {
        let def = self.def;
        let exe_path = self.exe_path.clone();
        desired_hooks(def, move |event| def.hook_command(&exe_path, event))
    }

    /// Plans the install against the real files: parse, merge, compute
    /// trust tables — no writes.
    pub fn plan_install(&self) -> Result<CodexInstallPlan, CodexInstallError> {
        let hooks_json_path = self.hooks_json_path();
        let config_toml_path = self.config_toml_path();
        if !config_dir_exists(self) && !self.def.create_config_dir_if_missing {
            return Err(CodexInstallError::ConfigMissing(
                "Required agent configuration is missing. Run `cmux hooks setup` after installing your agent CLI."
                    .to_owned(),
            ));
        }
        let existing = read_hooks_json(&hooks_json_path)?;
        let desired = self.desired_hooks();
        let (merged, outcome) = install_hooks_json(&existing, self.def, &desired, true);
        if outcome == MergeOutcome::Unchanged {
            return Ok(CodexInstallPlan::UpToDate);
        }
        let hooks_content =
            serde_json::to_string_pretty(&merged).unwrap_or_else(|_| "{}".to_owned());
        // Trust tables are computed on the MERGED hooks (upstream computes
        // them on the merged dict too).
        let mut trust_entries = codex_trust_entries(&merged, self.def, &hooks_json_path, false);
        // The marker-block editor only ever touches cmux-owned content, so
        // an existing config.toml gets the trust tables; a missing one stays
        // untouched (upstream `postInstallAction` guard).
        let config_toml_existed = config_toml_path.exists();
        if !config_toml_existed {
            trust_entries.clear();
        }
        let existing_config = std::fs::read_to_string(&config_toml_path).unwrap_or_default();
        let prefixes = codex_trust_key_prefixes(&hooks_json_path);
        let InstallResult {
            content: config_content,
            ..
        } = installing_hooks(&existing_config, &trust_entries, &prefixes, &[]);
        Ok(CodexInstallPlan::Write {
            hooks_json_path,
            config_toml_path,
            hooks_content,
            config_content,
            config_toml_existed,
        })
    }

    /// Plans the uninstall against the real files.
    pub fn plan_uninstall(&self) -> Result<CodexUninstallPlan, CodexInstallError> {
        let hooks_json_path = self.hooks_json_path();
        if !hooks_json_path.exists() {
            return Ok(CodexUninstallPlan::NothingToUninstall);
        }
        let existing = read_hooks_json(&hooks_json_path)?;
        // ALL path-derived trust context must be computed before the plan
        // below consumes `hooks_json_path`.
        let prefixes = codex_trust_key_prefixes(&hooks_json_path);
        let (merged, removed) = uninstall_hooks_json(&existing, self.def, true);
        // Trust entries to remove are computed from the PRE-PRUNE hooks
        // (includeLegacy true) plus the stale hashes of the current form.
        let existing_trust = codex_trust_entries(&existing, self.def, &hooks_json_path, true);
        let mut current = serde_json::Map::new();
        current.insert(
            "hooks".to_owned(),
            serde_json::Value::Object(self.desired_hooks()),
        );
        let current_trust = codex_trust_entries(
            &serde_json::Value::Object(current),
            self.def,
            &hooks_json_path,
            false,
        );
        let stale_hashes: Vec<String> = current_trust
            .iter()
            .map(|e| e.trusted_hash.clone())
            .collect();
        let hooks_content =
            serde_json::to_string_pretty(&merged).unwrap_or_else(|_| "{}".to_owned());
        let mut content_layout = CodexUninstallPlan::Remove {
            hooks_json_path,
            hooks_content,
            removed,
            config_toml: None,
        };
        let config_toml_path = self.config_toml_path();
        if config_toml_path.exists() {
            let source = std::fs::read_to_string(&config_toml_path).map_err(|e| {
                CodexInstallError::Io(format!(
                    "{:#} exists but could not be read. Fix permissions or remove it before uninstalling Codex hooks. {e}",
                    config_toml_path.display()
                ))
            })?;
            let config_content =
                uninstalling_hooks(&source, &existing_trust, &prefixes, &stale_hashes);
            content_layout = match content_layout {
                CodexUninstallPlan::Remove {
                    hooks_json_path,
                    hooks_content,
                    removed,
                    ..
                } => CodexUninstallPlan::Remove {
                    hooks_json_path,
                    hooks_content,
                    removed,
                    config_toml: Some((config_toml_path, config_content)),
                },
                other => other,
            };
        }
        Ok(content_layout)
    }
}

fn config_dir_exists(installer: &CodexHooks) -> bool {
    installer.config_dir.is_dir()
}

/// Reads hooks.json into a JSON object value (empty object when absent).
fn read_hooks_json(path: &Path) -> Result<serde_json::Value, CodexInstallError> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let source =
        std::fs::read_to_string(path).map_err(|e| CodexInstallError::Io(format!("{e}")))?;
    let value: serde_json::Value = serde_json::from_str(&source).map_err(|e| {
        CodexInstallError::InvalidHooksJson(format!(
            "{} exists but is not valid JSON. Fix or remove it before installing hooks. {e}",
            path.display()
        ))
    })?;
    if !value.is_object() {
        return Err(CodexInstallError::InvalidHooksJson(format!(
            "{} must be a JSON object. Fix or remove it before installing hooks.",
            path.display()
        )));
    }
    Ok(value)
}

/// What `cmux hooks codex uninstall` should do.
#[derive(Debug, Clone, PartialEq)]
pub enum CodexUninstallPlan {
    /// No hooks.json present.
    NothingToUninstall,
    /// Write these contents; `removed` counts the pruned hook entries.
    Remove {
        hooks_json_path: PathBuf,
        hooks_content: String,
        removed: usize,
        /// `Some((path, content))` when config.toml exists and changed.
        config_toml: Option<(PathBuf, String)>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cmux-hooks-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".codex")).expect("scratch dir");
        dir
    }

    fn installer(dir: &Path) -> CodexHooks {
        CodexHooks::with_config_dir(
            hook_def("codex").expect("codex def"),
            dir.join(".codex"),
            Some(PathBuf::from(r"C:\build\cmux.exe")),
        )
    }

    #[test]
    fn missing_agent_config_is_reported_not_created() {
        let dir = std::env::temp_dir().join(format!("cmux-hooks-absent-{}-1", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let hooks = installer(&dir);
        let plan = hooks.plan_install();
        assert!(
            matches!(plan, Err(CodexInstallError::ConfigMissing(_))),
            "{plan:?}"
        );
    }

    #[test]
    fn fresh_plan_writes_three_events_and_trust() {
        let dir = scratch("fresh");
        // config.toml exists (a real codex installation has one).
        fs::write(
            dir.join(".codex").join("config.toml"),
            "model = \"gpt-5\"\n",
        )
        .expect("config");
        let hooks = installer(&dir);
        let plan = hooks.plan_install().expect("plan");
        let CodexInstallPlan::Write {
            hooks_content,
            config_content,
            config_toml_existed,
            ..
        } = &plan
        else {
            panic!("expected a write plan, got {plan:?}");
        };
        assert!(*config_toml_existed);
        let parsed: serde_json::Value = serde_json::from_str(hooks_content).expect("parse");
        assert_eq!(
            parsed.pointer("/hooks").unwrap().as_object().unwrap().len(),
            3
        );
        assert!(config_content.contains("hooks = true"));
        assert!(config_content.contains("[hooks.state."));
        assert!(config_content.contains("trusted_hash = \"sha256:"));
        // The user's own config content survives the marker-block edit.
        assert!(config_content.contains("model = \"gpt-5\""));
    }

    #[test]
    fn fresh_plan_leaves_a_missing_config_toml_alone() {
        let dir = scratch("no-config");
        let hooks = installer(&dir);
        let plan = hooks.plan_install().expect("plan");
        let CodexInstallPlan::Write {
            config_toml_existed,
            config_toml_path,
            ..
        } = &plan
        else {
            panic!("expected a write plan, got {plan:?}");
        };
        assert!(!*config_toml_existed);
        assert!(!config_toml_path.exists(), "plan must not create files");
        // The content is still computed (for display) but the exe that
        // applies the plan must skip the write.
    }

    #[test]
    fn full_cycle_install_then_uninstall_removes_everything_cmux_wrote() {
        let dir = scratch("cycle");
        let hooks = installer(&dir);
        let CodexInstallPlan::Write {
            hooks_content,
            config_content,
            hooks_json_path,
            config_toml_path,
            ..
        } = hooks.plan_install().expect("install plan")
        else {
            panic!("fresh install must plan writes");
        };
        fs::write(&hooks_json_path, &hooks_content).expect("write hooks");
        fs::write(&config_toml_path, &config_content).expect("write config");

        // Reinstall is a no-op against the written files.
        let hooks = installer(&dir);
        assert_eq!(
            hooks.plan_install().expect("replan"),
            CodexInstallPlan::UpToDate
        );

        let CodexUninstallPlan::Remove {
            hooks_content: removed_hooks,
            removed,
            config_toml,
            ..
        } = hooks.plan_uninstall().expect("uninstall plan")
        else {
            panic!("expected removal plan");
        };
        assert_eq!(removed, 3, "one hook entry per event");
        let parsed: serde_json::Value = serde_json::from_str(&removed_hooks).expect("parse");
        assert!(parsed.pointer("/hooks").is_none(), "{parsed}");
        assert!(
            config_toml.is_some(),
            "config.toml was written, must shrink"
        );
    }

    #[test]
    fn invalid_hooks_json_is_reported_actionably() {
        let dir = scratch("invalid");
        fs::write(dir.join(".codex").join("hooks.json"), "{ not json").expect("write");
        let hooks = installer(&dir);
        let plan = hooks.plan_install();
        assert!(
            matches!(plan, Err(CodexInstallError::InvalidHooksJson(_))),
            "{plan:?}"
        );
    }

    #[test]
    fn trust_keys_escape_backslashes_and_stay_path_stable() {
        let dir = scratch("keys");
        let hooks = installer(&dir);
        let path = hooks.hooks_json_path();
        let key = trust_key_source_path(&path);
        assert!(key.ends_with("hooks.json"), "{key:?}");
        let prefixes = codex_trust_key_prefixes(&path);
        assert_eq!(prefixes.len(), 1);
        assert!(prefixes[0].ends_with(':'), "{prefixes:?}");
    }
}
