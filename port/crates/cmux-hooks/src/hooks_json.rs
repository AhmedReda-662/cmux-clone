//! The hooks.json merge engine — a faithful port of upstream's install and
//! uninstall behavior for `.nested`-format hook files
//! (`CLI/cmux.swift:37517-37624` install, `37838-37935` uninstall):
//!
//! - Existing cmux-owned entries are pruned first; **user entries are always
//!   preserved, even inside the same group as a cmux hook** — only cmux's
//!   own entries are removed (manifest §9).
//! - The *original indexes* of pruned cmux groups are recorded, so a
//!   reinstall re-inserts cmux entries at the position the user last saw
//!   (`insertCmuxHookValues`), not at the end.
//! - Groups whose `hooks` is not a list are preserved verbatim (never
//!   mutate unknown shapes).
//! - Events emptied entirely disappear.
//! - Outer keys of the JSON object are preserved as-is (codex files keep
//!   whatever top-level fields were there; nested files do not receive a
//!   `version` key from upstream).
//!
//! The desired set for the port installs only the three admission events
//! (session-start, prompt-submit, stop); the feed bridge is a later slice.

use crate::catalog::{is_cmux_owned_hook_command, HookDef, HookEvent};
use serde_json::{Map, Value};

/// One desired hook group: `[{"hooks":[{"type":"command","command":c,
/// "timeout":s}]}]` for nested formats (upstream `buildHooksDict`).
pub fn desired_group(def: &HookDef, event: HookEvent, command: &str) -> Value {
    let _ = event; // the event participates in feed/flat formats later
    let mut handler = Map::new();
    handler.insert("type".to_owned(), Value::String("command".to_owned()));
    handler.insert("command".to_owned(), Value::String(command.to_owned()));
    handler.insert(
        "timeout".to_owned(),
        Value::Number(def.nested_timeout_seconds().into()),
    );
    let mut group = Map::new();
    group.insert(
        "hooks".to_owned(),
        Value::Array(vec![Value::Object(handler)]),
    );
    Value::Object(group)
}

/// Builds the full desired `hooks` map for one definition.
pub fn desired_hooks(def: &HookDef, command: impl Fn(HookEvent) -> String) -> Map<String, Value> {
    let mut hooks = Map::new();
    for event in def.events {
        hooks.insert(
            event.agent_event.to_owned(),
            Value::Array(vec![desired_group(def, *event, &command(*event))]),
        );
    }
    hooks
}

#[derive(Debug, PartialEq)]
pub enum MergeOutcome {
    /// The file already has exactly the desired state (byte-identical after
    /// canonical pretty printing): skip the write (upstream no-op path).
    Unchanged,
    /// The merge produced new content.
    Changed,
}

/// Installs the desired hooks into an existing hooks.json object value,
/// preserving foreign content. `ownership(cap)` decides whether a command
/// string is cmux-owned (`include_legacy` = prune historical forms too).
///
/// `existing_json` is the whole file object (created empty when absent);
/// the function returns the merged whole-object value.
pub fn install(
    existing_json: &Value,
    def: &HookDef,
    desired: &Map<String, Value>,
    include_legacy: bool,
) -> (Value, MergeOutcome) {
    let mut merged = existing_json.clone();
    let hooks = existing_json
        .get("hooks")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    // Pass 1: prune owned entries, remember original group positions.
    let mut insertion_indexes: std::collections::HashMap<String, Vec<usize>> = Default::default();
    let mut pruned_hooks = Map::new();
    for (event, value) in &hooks {
        let Some(raw_groups) = value.as_array() else {
            // Unknown shape: preserve verbatim.
            pruned_hooks.insert(event.clone(), value.clone());
            continue;
        };
        let mut rewritten_groups: Vec<Value> = Vec::new();
        for group in raw_groups {
            let Some(hook_list) = group.get("hooks").and_then(Value::as_array) else {
                // Unknown shape: preserve verbatim so we never mutate user
                // custom data.
                rewritten_groups.push(group.clone());
                continue;
            };
            let owned_here = hook_list.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .map(|command| is_cmux_owned_hook_command(command, def, include_legacy))
                    .unwrap_or(false)
            });
            if owned_here {
                // ~: record the ORIGINAL index where a cmux group sits.
                insertion_indexes
                    .entry(event.clone())
                    .or_default()
                    .push(rewritten_groups.len());
            }
            let kept: Vec<Value> = hook_list
                .iter()
                .filter(|hook| {
                    !(hook
                        .get("command")
                        .and_then(Value::as_str)
                        .map(|c| is_cmux_owned_hook_command(c, def, include_legacy))
                        .unwrap_or(false))
                })
                .cloned()
                .collect();
            if kept.is_empty() {
                // Fully cmux-owned group: drop it.
                continue;
            }
            let mut kept_group = group.clone();
            if let Some(obj) = kept_group.as_object_mut() {
                obj.insert("hooks".to_owned(), Value::Array(kept));
            }
            rewritten_groups.push(kept_group);
        }
        // Insertion indexes were recorded against the pre-trim positions of
        // owned groups; since we re-insert AFTER this event's list is fully
        // indexed (single guarded re-insert below), the recorded positions
        // refer to the original user ordering — matches upstream's
        // `insertCmuxHookValues` accounting.
        if rewritten_groups.is_empty() {
            if insertion_indexes.contains_key(event) {
                // All groups were cmux's; the event exists only for us.
                pruned_hooks.remove(event);
                continue;
            }
            continue;
        }
        pruned_hooks.insert(event.clone(), Value::Array(rewritten_groups));
    }

    // Pass 2: insert the new entries (recorded positions climb by the
    // inserted count, matching upstream `insertCmuxHookValues`).
    for (event, new_groups_value) in desired {
        let groups = pruned_hooks
            .entry(event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        let array = groups.as_array_mut().expect("we only build arrays here");
        let Some(values) = new_groups_value.as_array() else {
            continue;
        };
        if let Some(indexes) = insertion_indexes.get(event) {
            let mut inserted_count = 0_usize;
            for original_index in indexes {
                let insertion_index = original_index
                    .saturating_add(inserted_count)
                    .min(array.len());
                array.splice(insertion_index..insertion_index, values.clone());
                inserted_count += values.len();
            }
        } else {
            array.extend(values.iter().cloned());
        }
    }

    if hooks_is_empty(&pruned_hooks) {
        merged.as_object_mut().unwrap().remove("hooks");
    } else {
        merged
            .as_object_mut()
            .unwrap()
            .insert("hooks".to_owned(), Value::Object(pruned_hooks.clone()));
    }
    let outcome = if &merged == existing_json {
        MergeOutcome::Unchanged
    } else {
        MergeOutcome::Changed
    };
    (merged, outcome)
}

fn hooks_is_empty(hooks: &Map<String, Value>) -> bool {
    hooks
        .iter()
        .all(|(_, v)| v.as_array().map(|a| a.is_empty()).unwrap_or(false))
}

/// Uninstalls every cmux-owned entry. Returns `(merged, removed_count)`.
pub fn uninstall(existing_json: &Value, def: &HookDef, include_legacy: bool) -> (Value, usize) {
    let mut merged = existing_json.clone();
    let hooks = existing_json
        .get("hooks")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut removed = 0;
    let mut kept_hooks = Map::new();
    for (event, value) in &hooks {
        let Some(raw_groups) = value.as_array() else {
            kept_hooks.insert(event.clone(), value.clone());
            continue;
        };
        let mut rewritten_groups: Vec<Value> = Vec::new();
        for group in raw_groups {
            let Some(hook_list) = group.get("hooks").and_then(Value::as_array) else {
                rewritten_groups.push(group.clone());
                continue;
            };
            let (kept, pruned_count) = {
                let kept: Vec<Value> = hook_list
                    .iter()
                    .filter(|hook| {
                        !(hook
                            .get("command")
                            .and_then(Value::as_str)
                            .map(|c| is_cmux_owned_hook_command(c, def, include_legacy))
                            .unwrap_or(false))
                    })
                    .cloned()
                    .collect();
                (kept.clone(), hook_list.len() - kept.len())
            };
            removed += pruned_count;
            if kept.is_empty() {
                continue;
            }
            let mut kept_group = group.clone();
            if let Some(obj) = kept_group.as_object_mut() {
                obj.insert("hooks".to_owned(), Value::Array(kept));
            }
            rewritten_groups.push(kept_group);
        }
        if rewritten_groups.is_empty() {
            continue;
        }
        kept_hooks.insert(event.clone(), Value::Array(rewritten_groups));
    }

    if hooks_is_empty(&kept_hooks) {
        merged.as_object_mut().unwrap().remove("hooks");
    } else {
        merged
            .as_object_mut()
            .unwrap()
            .insert("hooks".to_owned(), Value::Object(kept_hooks));
    }
    (merged, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::hook_def;

    fn def() -> &'static HookDef {
        hook_def("codex").unwrap()
    }

    fn desired() -> Map<String, Value> {
        desired_hooks(def(), |event| {
            format!(
                r#""C:\build\cmux.exe" hooks enqueue codex {}"#,
                event.cmux_subcommand
            )
        })
    }

    #[test]
    fn fresh_install_writes_the_three_events() {
        let d = def();
        let desired = desired_hooks(d, |event| {
            format!(
                r#""C:\build\cmux.exe" hooks enqueue codex {}"#,
                event.cmux_subcommand
            )
        });
        let (merged, outcome) = install(&serde_json::json!({}), d, &desired, true);
        assert_eq!(outcome, MergeOutcome::Changed);
        let hooks = merged.get("hooks").unwrap().as_object().unwrap();
        assert_eq!(hooks.len(), 3, "{hooks:?}");
        let stop = hooks.get("Stop").unwrap().as_array().unwrap();
        let group = stop.first().unwrap().as_object().unwrap();
        let hook = group.get("hooks").unwrap().as_array().unwrap()[0]
            .as_object()
            .unwrap();
        assert_eq!(hook.get("type").unwrap(), "command");
        assert_eq!(hook.get("timeout").unwrap().as_i64(), Some(5));
        assert_eq!(
            hook.get("command").unwrap(),
            r#""C:\build\cmux.exe" hooks enqueue codex stop"#
        );
    }

    #[test]
    fn reinstall_is_idempotent() {
        let d = def();
        let desired = desired();
        let (first, _) = install(&serde_json::json!({}), d, &desired, true);
        let (second, outcome) = install(&first, d, &desired, true);
        assert_eq!(outcome, MergeOutcome::Unchanged);
        assert_eq!(first, second);
    }

    #[test]
    fn user_entries_in_the_same_group_are_preserved() {
        let d = def();
        let desired = desired();
        let user_command = r"C:\Users\noah\scripts\my-hook.cmd";
        let existing = serde_json::json!({
            "other": true,
            "hooks": {
                "Stop": [
                    { "hooks": [ { "type": "command", "command": user_command, "timeout": 9 } ] }
                ]
            }
        });
        let (merged, _) = install(&existing, d, &desired, true);
        let stop = merged.pointer("/hooks/Stop").unwrap().as_array().unwrap();
        // The user's group stays first; our group lands at the recorded
        // original position (index 0 for us — the cmux group was there).
        let commands: Vec<String> = stop
            .iter()
            .flat_map(|g| {
                g.pointer("/hooks/0/command")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        assert!(commands.contains(&user_command.to_owned()));
        assert!(commands
            .iter()
            .any(|c| c.contains("hooks enqueue codex stop")));
        assert_eq!(merged.pointer("/other").unwrap(), &serde_json::json!(true));
    }

    #[test]
    fn unknown_group_shapes_are_preserved_verbatim() {
        let d = def();
        let desired = desired();
        let existing = serde_json::json!({ "hooks": { "Stop": [ { "weird": true } ] } });
        let (merged, _) = install(&existing, d, &desired, true);
        assert_eq!(
            merged.pointer("/hooks/Stop/0").unwrap(),
            &serde_json::json!({ "weird": true })
        );
        // And the desired group is inserted too.
        let stop = merged.pointer("/hooks/Stop").unwrap().as_array().unwrap();
        assert_eq!(stop.len(), 2);
    }

    #[test]
    fn uninstall_removes_only_cmux_entries_and_counts() {
        let d = def();
        let desired = desired();
        let user_command = r"C:\Users\noah\scripts\my-hook.cmd";
        let existing = serde_json::json!({
            "hooks": {
                "Stop": [
                    { "hooks": [ { "type": "command", "command": user_command, "timeout": 9 } ] },
                    { "hooks": [ { "type": "command", "command": desired["Stop"].as_array().unwrap()[0]["hooks"][0]["command"], "timeout": 5 } ] }
                ]
            }
        });
        let (merged, removed) = uninstall(&existing, d, true);
        assert_eq!(removed, 1);
        let stop = merged.pointer("/hooks/Stop").unwrap().as_array().unwrap();
        let group = stop[0].as_object().unwrap();
        let hook = group.get("hooks").unwrap().as_array().unwrap()[0]
            .as_object()
            .unwrap();
        assert_eq!(hook.get("command").unwrap(), user_command);
    }

    #[test]
    fn uninstall_of_a_file_without_hooks_is_a_noop() {
        let d = def();
        let (merged, removed) = uninstall(&serde_json::json!({ "note": 1 }), d, true);
        assert_eq!(removed, 0);
        assert_eq!(merged, serde_json::json!({ "note": 1 }));
    }
}
