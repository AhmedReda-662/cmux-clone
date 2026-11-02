//! cmux-ipc — the port's local-control transport, first slice (ADR-0005):
//!
//! - **Transport**: one byte-mode duplex named pipe per moment
//!   (`named_pipes` via the `windows` crate's documented wrappers), with the
//!   `cmuxOnly`-class security default: a protected DACL for the current
//!   user's SID plus a process-tree-descendant check on the client PID —
//!   the Windows analog of upstream's `LOCAL_PEERCRED`/`LOCAL_PEERPID`
//!   gates.
//! - **Protocol**: unchanged v2 JSON-lines shape
//!   (`{"id","method","params"}` → `{"id","ok",…}`); this slice carries
//!   `ping` and `agent.hook.enqueue` (M3.3 hook admission).
//!
//! Everything OS-specific lives in this crate behind [`server`]/[`client`];
//! the app and the CLI consume validated JSON values only
//! (manifest §7/§11: Windows specifics stay behind clear interfaces; IPC
//! requests are validated before dispatch).

pub mod ancestry;
pub mod client;
pub mod password;
pub mod protocol;
pub mod sddl;
pub mod server;

pub use client::{
    request, request_with_auth, Connection, IpcError, DEFAULT_CONNECT_BUDGET,
    DEFAULT_RESPONSE_BUDGET,
};
pub use protocol::{
    capabilities_result, get_wire_name, is_wire_name, method_route, parse_app_request,
    problem_error, validate_enqueue_params, AppRequest, ElementTarget, GetField, IsState, KeyHalf,
    LoadState, MethodRoute, RequestProblem, ValidatedEnqueue, APP_METHODS, DEFAULT_WAIT_MS,
    MAX_WAIT_MS,
};
pub use server::{
    default_password_provider, spawn_config, spawn_server, AppCall, AuthMode, EnqueueRequest,
    HandlerFn, IpcCall, PasswordProvider, ServerConfig,
};

/// The per-user well-known pipe name: `\\.\pipe\cmux-<user-SID>`. The SID
/// (dash-separated, alnum only) keeps the name single-segment and unique
/// per account without inventing a scheme; `CMUX_SOCKET_PATH` always
/// overrides (upstream discovery order: `--socket` → `CMUX_SOCKET_PATH` →
/// known candidates — the flag lands with the M4 CLI surface).
pub fn default_pipe_name() -> std::io::Result<String> {
    let sid = sddl::current_user_sid()
        .map_err(|e| std::io::Error::other(format!("current user SID: {e}")))?;
    Ok(format!(r"\\.\pipe\cmux-{sid}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pipe_name_has_the_exact_shape() {
        let name = default_pipe_name().expect("SID lookups work in tests");
        assert!(name.starts_with(r"\\.\pipe\cmux-S-1-5-"), "{name}");
        assert!(
            !name[r"\\.\pipe\".len()..].contains('\\'),
            "single segment: {name}"
        );
    }
}
