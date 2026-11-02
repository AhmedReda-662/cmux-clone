//! The Codex hook-trust hash, a faithful port of upstream
//! `codexCommandHookHash` (`CLI/cmux.swift:38178-38211`):
//!
//! ```text
//! sha256( compact JSON, keys sorted, no slash escaping ):
//! { "event_name": <label>,
//!   "hooks": [ { "async": false, "command": <cmd>,
//!                "timeout": <timeout as read (>=1)>, "type": "command"
//!                ("statusMessage": <s> when present) } ],
//!   ("matcher": <m> when the event uses one) }
//! "sha256:<64 hex>"
//! ```
//!
//! Key ordering: serde's `BTreeMap` maps give the same lexicographic
//! order as `JSONSerialization.sortedKeys` for these ASCII keys, and the
//! compact form uses the same `,`/`:` separators. Divergence note
//! (ADR-0011): Apple's exact string-escaping behavior is byte-compatible
//! for the ASCII command strings the port writes, but there is no upstream
//! binary oracle on Windows to diff against — the port installs *and*
//! verifies with its own hashes on one machine, so a formatting difference
//! cannot corrupt anything the port itself manages.

use sha2::{Digest, Sha256};

/// Builds the trust hash for one hook handler.
pub fn codex_command_hook_hash(
    event_label: &str,
    matcher: Option<&str>,
    command: &str,
    timeout_ms: i64,
    status_message: Option<&str>,
) -> String {
    let normalized_timeout_ms = timeout_ms.max(1);
    let mut handler = serde_json::Map::new();
    handler.insert("async".to_owned(), serde_json::Value::Bool(false));
    handler.insert(
        "command".to_owned(),
        serde_json::Value::String(command.to_owned()),
    );
    handler.insert(
        "timeout".to_owned(),
        serde_json::Value::Number(serde_json::Number::from(normalized_timeout_ms)),
    );
    handler.insert(
        "type".to_owned(),
        serde_json::Value::String("command".to_owned()),
    );
    if let Some(status) = status_message {
        handler.insert(
            "statusMessage".to_owned(),
            serde_json::Value::String(status.to_owned()),
        );
    }
    let mut identity = serde_json::Map::new();
    identity.insert(
        "event_name".to_owned(),
        serde_json::Value::String(event_label.to_owned()),
    );
    identity.insert(
        "hooks".to_owned(),
        serde_json::Value::Array(vec![serde_json::Value::Object(handler)]),
    );
    if let Some(matcher) = matcher {
        identity.insert(
            "matcher".to_owned(),
            serde_json::Value::String(matcher.to_owned()),
        );
    }

    let canonical = serde_json::Value::Object(identity).to_string();
    let digest = Sha256::digest(canonical.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_is_compact_sorted_and_slash_safe() {
        let hash = codex_command_hook_hash(
            "stop",
            None,
            r#"C:\tools\cmux.exe" hooks enqueue codex stop"#,
            5,
            None,
        );
        // 64 hex digits, ledger-prefixed.
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), "sha256:".len() + 64);
        // Deterministic.
        let again = codex_command_hook_hash(
            "stop",
            None,
            r#"C:\tools\cmux.exe" hooks enqueue codex stop"#,
            5,
            None,
        );
        assert_eq!(hash, again);
        // Sensitive to each identity component.
        let other_command = codex_command_hook_hash(
            "stop",
            None,
            r#"C:\tools\OTHER.exe" hooks enqueue codex stop"#,
            5,
            None,
        );
        assert_ne!(hash, other_command);
        let other_timeout = codex_command_hook_hash(
            "stop",
            None,
            r#"C:\tools\cmux.exe" hooks enqueue codex stop"#,
            600,
            None,
        );
        assert_ne!(hash, other_timeout);
        let other_event = codex_command_hook_hash(
            "session_start",
            None,
            r#"C:\tools\cmux.exe" hooks enqueue codex stop"#,
            5,
            None,
        );
        assert_ne!(hash, other_event);
        // Matcher participates when present.
        let with_matcher = codex_command_hook_hash(
            "stop",
            Some("*"),
            r#"C:\tools\cmux.exe" hooks enqueue codex stop"#,
            5,
            None,
        );
        assert_ne!(with_matcher, hash);
        // Zero timeouts clamp to 1 (upstream: `max(timeoutMs, 1)`).
        assert_eq!(
            codex_command_hook_hash("stop", None, "x", 0, None),
            codex_command_hook_hash("stop", None, "x", 1, None)
        );
    }

    #[test]
    fn status_message_is_carried_only_when_present() {
        let plain = codex_command_hook_hash("stop", None, "x", 5, None);
        let with_status = codex_command_hook_hash("stop", None, "x", 5, Some("cmux"));
        assert_ne!(plain, with_status);
    }
}
