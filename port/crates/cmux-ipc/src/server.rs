//! The pipe **server**: the name holds `PIPE_INSTANCES` byte-mode duplex
//! instances, each accepted and served on its own thread (M4.4 — upstream's
//! "a connection pool" admission contract: no one client can hold the pipe,
//! a connected client may still speak multiple request lines, each answered
//! in order, until it hangs up; ADR-0005 transport + Amendment 4). Beyond
//! the instance count a client fails at the open (ERROR_PIPE_BUSY) and the
//! shared client loop retries within its connect budget — the port never
//! answers in-band `overloaded` from this seam.
//!
//! Security model (ADR-0005 Amendment 3, upstream `SocketControlMode`
//! minus `off`/`allowAll`):
//! 1. the pipe DACL (`sddl::pipe_sddl_for_current_user`) rejects every
//!    other user's clients at the open — unchanged in every mode;
//! 2. `CmuxOnly` (upstream default) additionally runs the
//!    `ancestry::is_descendant_of` process-tree gate on the client PID;
//!    `Automation`/`Password` admit any same-user client (upstream grads
//!    them by same-UID alone);
//! 3. `Password` puts the login handshake before dispatch (`auth.login`
//!    V2, plain `auth <pw>` V1) and revokes a connection whose verified
//!    fingerprint stops matching the live credential;
//! 4. every request is still validated (`protocol::*`) before dispatch.
//!
//! The server never executes commands: it answers the static probes
//! (`ping`/`system.ping`, `system.capabilities`, `auth.login`), admits
//! hook events, and forwards validated app-plane calls (`IpcCall::App`)
//! to the handler — the app executes them on its frame loop and answers
//! within the reply budget; a missing answer becomes a structured
//! `timeout` error.

use std::io::{BufReader, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::Arc;

use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    FlushFileBuffers, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};

use crate::ancestry;
use crate::protocol::{self, AppRequest, MethodRoute, RequestProblem, MAX_REQUEST_BYTES};
use crate::sddl;

/// The pipe's shared security descriptor, made transferable across the
/// server's pool threads.
///
/// `PSECURITY_DESCRIPTOR` is a bare `*mut c_void`, so it is naturally
/// `!Send`/`!Sync`. Here the descriptor is allocated once on the server
/// thread and only ever read: each pool thread hands the same pointer to
/// `CreateNamedPipeW`, which copies the descriptor into its pipe instance
/// (documented Win32 behavior) and works on its own copy; nobody mutates
/// or frees it off the owning thread. The wrapper therefore restores
/// pointer-neutrality for this specific sharing pattern and adds no
/// unchecked access.
#[derive(Clone, Copy)]
struct SharedSecurityDescriptor(PSECURITY_DESCRIPTOR);

unsafe impl Send for SharedSecurityDescriptor {}
unsafe impl Sync for SharedSecurityDescriptor {}

#[derive(Debug)]
pub enum ServerStartError {
    /// The name is already owned by another instance (a second cmux
    /// window): the caller disables IPC instead of fighting.
    NameInUse(String),
    Security(windows::core::Error),
    Create(String),
}

impl std::fmt::Display for ServerStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerStartError::NameInUse(name) => write!(f, "pipe name already in use: {name}"),
            ServerStartError::Security(e) => write!(f, "pipe security: {e}"),
            ServerStartError::Create(what) => write!(f, "pipe create failed: {what}"),
        }
    }
}

/// One validated request handed to the app's handler: `(id, agent,
/// normalized_enqueue)` for `agent.hook.enqueue`. Static probes
/// (`ping`/`capabilities`) never reach the handler (the server answers
/// itself).
#[derive(Debug)]
pub struct EnqueueRequest {
    pub id: serde_json::Value,
    pub validated: crate::protocol::ValidatedEnqueue,
}

/// Spawn the serving loop on its own thread. `handler` receives every
/// validated enqueue and returns the JSON `result` value to echo back
/// (most callers reply `{"queued":true}` or `{"queued":false}`).
/// The server→app seam. Serve one `IpcCall`; the reply value/Problem is
/// wrapped into the wire envelope by the server (`None` means "no answer":
/// enqueue falls back to `{"queued":true}`, app calls become `timeout`).
/// The pool shares one handler across every connection thread, so the
/// closure must be `Sync` (M4.4).
pub type HandlerFn =
    Arc<dyn Fn(IpcCall) -> Option<Result<serde_json::Value, RequestProblem>> + Send + Sync>;

/// One app-plane call forwarded to the handler: the correlated id plus the
/// validated request (M4.1 request plane; the app answers on its frame
/// loop — see cmux-shell's `poll_children` drain).
#[derive(Debug)]
pub struct AppCall {
    pub id: serde_json::Value,
    pub request: AppRequest,
}

/// Everything the server can hand off: the M3.3 hook admission or an M4.1
/// app-plane call.
#[derive(Debug)]
pub enum IpcCall {
    Enqueue(EnqueueRequest),
    App(AppCall),
}

pub fn spawn_server(
    pipe_name: String,
    require_descendant: bool,
    handler: HandlerFn,
) -> std::thread::JoinHandle<()> {
    let mode = if require_descendant {
        AuthMode::CmuxOnly
    } else {
        AuthMode::Automation
    };
    spawn_config(
        ServerConfig {
            pipe_name,
            mode,
            password: default_password_provider(),
        },
        handler,
    )
}

/// The access mode (ADR-0005 Amendment 3; upstream `SocketControlMode`
/// minus `off`/`allowAll` — a started server is never `off`, and the
/// wide-open `allowAll` mode has no consumer yet):
/// - `CmuxOnly`: user-SID DACL **plus** the process-tree ancestry gate;
/// - `Automation`: user-SID DACL only — any same-user client may connect
///   (upstream `.automation`: same-UID gate only);
/// - `Password`: `Automation`'s admission plus the pre-dispatch password
///   handshake and per-command credential rotation checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    CmuxOnly,
    Automation,
    Password,
}

impl AuthMode {
    /// Whether commands require a completed `auth.login` handshake
    /// (upstream `SocketControlMode.requiresPasswordAuth`).
    pub fn requires_password(&self) -> bool {
        matches!(self, AuthMode::Password)
    }
}

/// Reads the current configured password for one check. Called live on
/// every gate/rotation/login read so env/file edits take effect without a
/// restart (upstream re-reads its store per verification). Shared across
/// the pool's connection threads.
pub type PasswordProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// The production provider: explicit favorite (unused here) plus the real
/// environment and the default password file.
pub fn default_password_provider() -> PasswordProvider {
    Arc::new(|| crate::password::configured_password(None))
}

/// Full server configuration for new callers.
#[derive(Clone)]
pub struct ServerConfig {
    /// Pipe name (`\\.\pipe\…`).
    pub pipe_name: String,
    /// Access mode → DACL stays user-SID in every mode; only the ancestry
    /// gate and the password handshake differ.
    pub mode: AuthMode,
    /// Live password source (consulted per check in password mode).
    pub password: PasswordProvider,
}

/// Spawn the serving loop with full configuration (M4.2).
pub fn spawn_config(config: ServerConfig, handler: HandlerFn) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("cmux-ipc-server".to_owned())
        .spawn(move || serve_loop(config, handler))
        .expect("spawning the IPC server thread must succeed")
}

fn serve_loop(config: ServerConfig, handler: HandlerFn) {
    let server_pid = ancestry::server_own_pid();
    // Create the security descriptor ONCE so all pipe instances share it
    // (Windows requires all instances of a named pipe to have identical DACLs).
    let security = prepare_security().expect("pipe security descriptor");
    // Claim the name on this thread first: `FILE_FLAG_FIRST_PIPE_INSTANCE`
    // fails while the name is owned (a second app window), and every pool
    // worker must come up only after our ownership exists — otherwise a
    // worker would add instances to the *foreign* pipe and serve foreign
    // clients with this server's config.
    let bind_handle = match unsafe { open_pipe_instance(&config.pipe_name, true, Some(security)) } {
        Ok(handle) => handle,
        Err(start) => {
            eprintln!("cmux-ipc: {start} — IPC disabled");
            return;
        }
    };
    // The pool: additional instances each accept on their own thread, so no
    // client's whole session blocks another's open (upstream's
    // connection-pool admission contract, M4.4).
    let shared_security = Arc::new(SharedSecurityDescriptor(security));
    for index in 1..PIPE_INSTANCES {
        let config = config.clone();
        let handler = Arc::clone(&handler);
        let security = Arc::clone(&shared_security);
        std::thread::Builder::new()
            .name(format!("cmux-ipc-pool-{index}"))
            .spawn(move || run_instance(&config, &handler, server_pid, security))
            .expect("spawning an IPC pool worker must succeed");
    }
    // The main thread serves instance 1 (its first client already waits on
    // the claimed handle), then keeps the instance-1 reopen loop.
    match serve_connected(&config, &handler, server_pid, bind_handle) {
        Ok(()) => {}
        Err(reason) => eprintln!("cmux-ipc: connection failed: {reason}"),
    }
    run_auth(&config, &handler, server_pid, shared_security);
}

/// Named-pipe instances the server holds at once, one connection thread
/// each. Upstream bounds its pool the same way (`pool_saturated` beyond
/// it); here saturation lives at the OS open (`ERROR_PIPE_BUSY`) and the
/// client's connect-budget retry loop turns it into a bounded wait rather
/// than an in-band reply (ADR-0005 Amendment 4, recorded divergence).
const PIPE_INSTANCES: usize = 8;

/// One pool instance's endless open→accept→serve cycle. Create failures
/// are retried (the claim is on the main thread; a worker's create can
/// only lose a brief race with it), connection failures are logged.
fn run_instance(
    config: &ServerConfig,
    handler: &HandlerFn,
    server_pid: u32,
    security: Arc<SharedSecurityDescriptor>,
) {
    loop {
        let handle = match unsafe { open_pipe_instance(&config.pipe_name, false, Some(security.0)) }
        {
            Ok(handle) => handle,
            Err(start) => {
                eprintln!("cmux-ipc: pool instance create failed: {start} — retrying");
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
        };
        match serve_connected(config, handler, server_pid, handle) {
            Ok(()) => {}
            Err(reason) => eprintln!("cmux-ipc: connection failed: {reason}"),
        }
    }
}

fn run_auth(
    config: &ServerConfig,
    handler: &HandlerFn,
    server_pid: u32,
    security: Arc<SharedSecurityDescriptor>,
) {
    // The main thread's continuation of instance 1 (the claim happened in
    // `serve_loop`; from here the instance behaves like every pool member
    // so the instance count stays constant). A misbehaving client must not
    // kill the pipe.
    run_instance(config, handler, server_pid, security);
}

fn prepare_security() -> Result<windows::Win32::Security::PSECURITY_DESCRIPTOR, ServerStartError> {
    let sddl = sddl::pipe_sddl_for_current_user().map_err(ServerStartError::Security)?;
    sddl::sddl_descriptor(&sddl).map_err(ServerStartError::Security)
}

/// Creates one byte-mode duplex instance of `pipe_name`. The instance
/// count is fixed per pipe name (`PIPE_INSTANCES`, matched across every
/// create); `claim` carries the global-exclusivity flag and only the
/// first create of a name uses it — later instances must not fail because
/// "the pipe already exists".
///
/// When `shared_descriptor` is `Some`, it is used for this instance and
/// ownership is NOT transferred (all instances share the one descriptor the
/// server created, so every instance of the pipe presents an identical
/// DACL). When `None`, a fresh descriptor is built from the SDDL string,
/// used for this one create, and freed right after the create returns
/// (standalone/test callers).
unsafe fn open_pipe_instance(
    pipe_name: &str,
    claim: bool,
    shared_descriptor: Option<PSECURITY_DESCRIPTOR>,
) -> Result<HANDLE, ServerStartError> {
    // Use shared descriptor if provided; otherwise create a new one.
    // For non-claiming instances, passing None lets Windows inherit the
    // pipe's existing security descriptor (required for additional instances).
    let (descriptor, mut attributes) = if let Some(desc) = shared_descriptor {
        (
            desc,
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: desc.0 as *mut _,
                bInheritHandle: false.into(),
            },
        )
    } else {
        let desc = prepare_security()?;
        (
            desc,
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: desc.0 as *mut _,
                bInheritHandle: false.into(),
            },
        )
    };
    let name_wide: Vec<u16> = pipe_name.encode_utf16().chain([0]).collect();
    let access = if claim {
        PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        PIPE_ACCESS_DUPLEX
    };
    let handle = CreateNamedPipeW(
        windows::core::PCWSTR(name_wide.as_ptr()),
        access,
        // Byte-mode: std reads/writes ordinary bytes back-to-back.
        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
        PIPE_INSTANCES as u32,    // one connection thread per instance
        64 * 1024,                // out-buffer: one response line
        MAX_REQUEST_BYTES as u32, // in-buffer: the request ceiling
        0,
        Some(&mut attributes as *const SECURITY_ATTRIBUTES),
    );
    // Only free the descriptor if we created it locally
    if shared_descriptor.is_none() {
        sddl::local_free_sddl(descriptor);
    }
    if handle == INVALID_HANDLE_VALUE {
        let error = GetLastError();
        if error == ERROR_ACCESS_DENIED {
            return Err(ServerStartError::NameInUse(pipe_name.to_owned()));
        }
        return Err(ServerStartError::Create(format!(
            "CreateNamedPipeW: Win32 error {}",
            error.0
        )));
    }
    Ok(handle)
}

/// Accepts, gates, serves, and disconnects one client on `handle`.
/// Consumes `handle`: on success the client is gone (disconnected), on
/// failure the handle is closed here so a reopen loop can start fresh.
fn serve_connected(
    config: &ServerConfig,
    handler: &HandlerFn,
    server_pid: u32,
    handle: HANDLE,
) -> Result<(), String> {
    // ConnectNamedPipe blocks until a client opens the pipe (or raced in
    // first: ERROR_PIPE_CONNECTED is success).
    unsafe {
        if let Err(error) = ConnectNamedPipe(handle, None) {
            if error.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                let _ = CloseHandle(handle);
                return Err(format!("ConnectNamedPipe: {error}"));
            }
        }
    }

    let mut client_pid = 0u32;
    unsafe {
        let _ = GetNamedPipeClientProcessId(handle, &mut client_pid);
    }
    let mut file = unsafe {
        std::fs::File::from(std::os::windows::io::OwnedHandle::from_raw_handle(
            handle.0 as _,
        ))
    };

    if config.mode == AuthMode::CmuxOnly && !ancestry::is_descendant_of(client_pid, server_pid) {
        eprintln!("cmux-ipc: rejected client PID {client_pid}: not a process-tree descendant");
        let reply = protocol::error(
            &serde_json::Value::Null,
            "unauthorized_peer",
            "the pipe client is not a cmux process-tree descendant",
            "run the CLI from inside a cmux pane",
        );
        write_reply(&mut file, handle, &reply);
        let _ = unsafe { DisconnectNamedPipe(handle) };
        return Ok(());
    }

    serve_one_client(&mut file, handle, config.mode, &config.password, handler);
    let _ = unsafe { DisconnectNamedPipe(handle) };
    Ok(())
}

/// Legacy shim (M3.3–M4.1 semantics): `require_descendant` maps to the
/// `CmuxOnly`/`Automation` modes and the store-backed password provider.
/// Test-facing only; production starts servers through `spawn_server`/
/// `spawn_config`. Tests drive a single instance each (no pool workers);
/// pool behavior is exercised through the `spawn_config`-based tests.
#[cfg(test)]
fn run(pipe_name: String, require_descendant: bool, handler: HandlerFn, server_pid: u32) {
    let mode = if require_descendant {
        AuthMode::CmuxOnly
    } else {
        AuthMode::Automation
    };
    let config = ServerConfig {
        pipe_name,
        mode,
        password: default_password_provider(),
    };
    let security = Arc::new(SharedSecurityDescriptor(
        prepare_security().expect("pipe security descriptor"),
    ));
    run_instance(&config, &handler, server_pid, security);
}

/// Serves a connected client until it hangs up: one JSON line in, one
/// reply out, repeat (M4.2). A connection also carries the password auth
/// state in password mode: the login handshake runs before dispatch and a
/// served command re-verifies the live credential (upstream behavior — a
/// rotated password disconnects an authenticated connection).
fn serve_one_client(
    file: &mut std::fs::File,
    handle: HANDLE,
    mode: AuthMode,
    provider: &PasswordProvider,
    handler: &HandlerFn,
) {
    let reader = match file.try_clone() {
        Ok(cloned) => BufReader::new(cloned),
        Err(error) => {
            eprintln!("cmux-ipc: pipe read setup failed: {error}");
            return;
        }
    };
    let mut reader = reader;
    let mut auth = crate::password::PasswordAuthorization::new();
    loop {
        // Bound each line read: one ceiling-sized request is all we admit
        // per line. An over-long unterminated line disconnects (fail-closed,
        // no id-addressable envelope may exist inside it).
        let mut line = Vec::new();
        {
            let mut bounded = (&mut reader).take(MAX_REQUEST_BYTES as u64 + 1);
            match std::io::BufRead::read_until(&mut bounded, b'\n', &mut line) {
                Ok(0) | Err(_) => return, // client hung up: fine
                Ok(_) => {}
            }
        }
        trim_line(&mut line);
        if line.len() > MAX_REQUEST_BYTES {
            // Over-ceiling request: drain nothing further, answer nothing
            // (no id-addressable envelope may exist in an unbounded prefix).
            return;
        }
        let request_line = String::from_utf8_lossy(&line).into_owned();
        let Some(reply) = next_reply(&request_line, mode, provider, handler, &mut auth) else {
            // No reply (non-envelope line like upstream, or a revoked
            // connection): the connection ends here.
            return;
        };
        write_reply(file, handle, &reply);
    }
}

/// Resolves the next reply for one request line, or `None` to disconnect
/// without answering. In password mode the login handshake and the
/// rotation check run before dispatch (upstream `authResponseIfNeeded` +
/// `socketAuthorizationIsCurrent` ordering).
fn next_reply(
    request_line: &str,
    mode: AuthMode,
    provider: &PasswordProvider,
    handler: &HandlerFn,
    auth: &mut crate::password::PasswordAuthorization,
) -> Option<String> {
    if mode.requires_password() {
        // Rotation: an authenticated connection whose stored fingerprint no
        // longer matches the current credential must not continue. Silence,
        // then disconnect (upstream closes the connection).
        if auth.is_authenticated() && !auth.permits_continuation(provider().as_deref()) {
            return None;
        }
        // Login probes answer directly, before anything dispatches.
        if let Some(reply) = password_login_v2(request_line, provider, auth) {
            return Some(reply);
        }
        if let Some(reply) = password_login_v1(request_line, provider, auth) {
            return Some(reply);
        }
        // Anything else from an unauthenticated connection is refused.
        if !auth.is_authenticated() {
            return Some(auth_required_reply(request_line));
        }
    } else {
        // Non-password modes still serve the login probe inline (upstream's
        // system coordinator ack: always ok, `required: false` since no
        // handshake precedes dispatch).
        match protocol::parse_request_line(request_line) {
            Ok(Some((id, method, _))) if method == "auth.login" => {
                return Some(protocol::success(
                    &id,
                    serde_json::json!({ "authenticated": true, "required": false }),
                ));
            }
            Ok(Some(_)) => {}
            Ok(None) => return None,
            Err(_) => {
                // Plain text line: the legacy V1 `auth <password>` probe
                // gets upstream's unknown-command failure, which the CLI
                // tolerates (it cannot know the mode before asking).
                if is_v1_auth_line(request_line) {
                    return Some("ERROR: Unknown command 'auth'".to_owned());
                }
            }
        }
    }
    Some(dispatch_request_line(request_line, handler))
}

/// The existing M4.1 dispatch for one parsed request line.
fn dispatch_request_line(request_line: &str, handler: &HandlerFn) -> String {
    let reply = match protocol::parse_request_line(request_line) {
        Err(problem) => protocol::error(
            &serde_json::Value::Null,
            "bad_request",
            &problem,
            "send {id,method,params} JSON lines",
        ),
        Ok(None) => {
            // Not an envelope object: no id to correlate a reply with.
            return String::new();
        }
        Ok(Some((id, method, params))) => match protocol::method_route(&method) {
            MethodRoute::Ping => protocol::success(&id, serde_json::json!({ "pong": true })),
            MethodRoute::Capabilities => protocol::success(&id, protocol::capabilities_result()),
            MethodRoute::Auth => {
                // Reached only in non-password modes (password mode answers
                // `auth.login` in its pre-dispatch gate).
                protocol::success(
                    &id,
                    serde_json::json!({ "authenticated": true, "required": false }),
                )
            }
            MethodRoute::Enqueue => match protocol::validate_enqueue_params(&params) {
                Err(problem) => {
                    protocol::error(&id, "invalid_params", &problem, "fix the parameters")
                }
                Ok(validated) => {
                    let request = EnqueueRequest {
                        id: id.clone(),
                        validated,
                    };
                    let result = match handler(IpcCall::Enqueue(request)) {
                        Some(Ok(value)) => value,
                        // The queue closure never rejects; a hang-up still
                        // answers queued so a hook never stalls (M3.3 rule).
                        Some(Err(_)) | None => serde_json::json!({ "queued": true }),
                    };
                    protocol::success(&id, result)
                }
            },
            MethodRoute::App => match protocol::parse_app_request(&method, &params) {
                Err(problem) => protocol::problem_error(&id, &problem),
                Ok(request) => {
                    let call = IpcCall::App(AppCall {
                        id: id.clone(),
                        request,
                    });
                    match handler(call) {
                        Some(Ok(result)) => protocol::success(&id, result),
                        Some(Err(problem)) => protocol::problem_error(&id, &problem),
                        // No answer within the caller's budget: window busy
                        // or quitting. A recoverable, structured error.
                        None => protocol::problem_error(&id, &RequestProblem::app_timeout()),
                    }
                }
            },
            MethodRoute::Unknown => protocol::error(
                &id,
                "unknown_method",
                &format!("method '{method}' is not served by this build"),
                "see system.capabilities for the served set",
            ),
        },
    };
    reply
}

/// The v2 login handshake (`auth.login`), upstream `passwordLoginV2ResponseIfNeeded`:
/// `invalid_params` on a missing `params.password`, `auth_unconfigured` with
/// no configured credential, `auth_failed` on a mismatch, success as
/// `{"authenticated":true}` (the coordinator's `required` field is only in
/// the non-password ack). Always claims the line in password mode.
fn password_login_v2(
    request_line: &str,
    provider: &PasswordProvider,
    auth: &mut crate::password::PasswordAuthorization,
) -> Option<String> {
    let (id, method, params) = match protocol::parse_request_line(request_line) {
        Ok(Some(parsed)) => parsed,
        Ok(None) | Err(_) => return None,
    };
    if method != "auth.login" {
        return None;
    }
    let Some(provided) = params
        .as_object()
        .and_then(|object| object.get("password"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        return Some(protocol::error(
            &id,
            "invalid_params",
            "auth.login requires params.password",
            "pass params.password as a string",
        ));
    };
    let Some(configured) = provider() else {
        return Some(protocol::error(
            &id,
            "auth_unconfigured",
            "Password mode is enabled but no socket password is configured in Settings.",
            "configure the socket password",
        ));
    };
    if !crate::password::constant_time_equals(&configured, &provided) {
        return Some(protocol::error(
            &id,
            "auth_failed",
            "Invalid password",
            "check the password and retry",
        ));
    }
    auth.authenticate(&provided);
    Some(protocol::success(
        &id,
        serde_json::json!({ "authenticated": true }),
    ))
}

/// V1 login probe (plain `auth <password>` lines, upstream
/// `passwordLoginV1ResponseIfNeeded`), with upstream's exact reply texts and
/// precedence (unconfigured beats missing-password).
fn password_login_v1(
    request_line: &str,
    provider: &PasswordProvider,
    auth: &mut crate::password::PasswordAuthorization,
) -> Option<String> {
    if !is_v1_auth_line(request_line) {
        return None;
    }
    let Some(configured) = provider() else {
        return Some(
            "ERROR: Password mode is enabled but no socket password is configured in Settings."
                .to_owned(),
        );
    };
    // Upstream: `auth` alone → empty; `"auth rest…"` keeps every byte after
    // the 5-char `"auth "` prefix untrimmed (verify compares raw bytes).
    let lowered = request_line.to_lowercase();
    let provided = if lowered == "auth" {
        ""
    } else {
        &request_line["auth ".len()..]
    };
    if provided.is_empty() {
        return Some("ERROR: Missing password. Usage: auth <password>".to_owned());
    }
    if !crate::password::constant_time_equals(&configured, provided) {
        return Some("ERROR: Invalid password".to_owned());
    }
    auth.authenticate(provided);
    Some("OK: Authenticated".to_owned())
}

/// Whether a plain text line is a legacy login probe (upstream:
/// `lowered == "auth" || lowered.hasPrefix("auth ")`).
fn is_v1_auth_line(request_line: &str) -> bool {
    let lowered = request_line.to_lowercase();
    lowered == "auth" || lowered.starts_with("auth ")
}

/// Every other command from an unauthenticated connection (upstream
/// `passwordAuthRequiredResponse`): JSON envelopes get the structured
/// `auth_required` error; plain or malformed lines get the V1 text so old
/// tooling can react the way it does to any `ERROR:` line.
fn auth_required_reply(request_line: &str) -> String {
    const MESSAGE: &str = "Authentication required. Send auth <password> first.";
    match protocol::parse_request_line(request_line) {
        Ok(Some((id, _, _))) => protocol::error(
            &id,
            "auth_required",
            MESSAGE,
            "log in with auth.login before other methods",
        ),
        _ => format!("ERROR: {MESSAGE}"),
    }
}

fn write_reply(file: &mut std::fs::File, handle: HANDLE, reply: &str) {
    if file.write_all(reply.as_bytes()).is_err() || file.write_all(b"\n").is_err() {
        return;
    }
    let _ = unsafe { FlushFileBuffers(handle) };
}

fn trim_line(line: &mut Vec<u8>) {
    while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
        line.pop();
    }
    if line.len() > MAX_REQUEST_BYTES {
        line.truncate(MAX_REQUEST_BYTES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{self, DEFAULT_RESPONSE_BUDGET};
    use std::time::Duration;

    fn unique_name(tag: &str) -> String {
        format!(r"\\.\pipe\cmux-ipc-test-{}-{tag}", std::process::id())
    }

    fn spawn_cmd_sleeping(seconds: u32) -> std::process::Child {
        use std::os::windows::process::CommandExt;
        let mut args = vec!["/C".to_owned(), "ping".to_owned(), "-n".to_owned()];
        args.push(seconds.to_string());
        args.extend(["127.0.0.1".to_owned(), ">nul".to_owned()]);
        std::process::Command::new("cmd.exe")
            .args(&args)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .expect("cmd.exe must be spawnable in tests")
    }

    #[test]
    fn serves_ping_enqueue_unknown_and_invalid_requests() {
        let name = unique_name("serves");
        let (tx, rx) = std::sync::mpsc::channel();
        let handler: HandlerFn = Arc::new(move |call| {
            let IpcCall::Enqueue(request) = call else {
                return None;
            };
            let _ = tx.send((request.id, request.validated.clone()));
            Some(Ok(serde_json::json!({ "queued": true })))
        });
        let server_pid = ancestry::server_own_pid();
        let name_for_server = name.clone();
        std::thread::spawn(move || run(name_for_server, false, handler, server_pid));

        // Wait until the server instance exists (client retries absorb it).
        let mut pong = None;
        for _ in 0..25 {
            match client::request(
                &name,
                r#"{"id":"p1","method":"ping","params":{}}"#,
                Duration::from_millis(250),
                DEFAULT_RESPONSE_BUDGET,
            ) {
                Ok(reply) => {
                    pong = Some(reply);
                    break;
                }
                Err(_) => continue,
            }
        }
        let pong = pong.expect("ping answers once the server thread is up");
        assert!(pong.contains(r#""pong":true"#), "{pong}");

        let reply = client::request(
            &name,
            r#"{"id":"e1","method":"agent.hook.enqueue","params":{"agent":"codex","subcommand":"stop","payload":{"cwd":"C:\\tmp"},"environment":{"CMUX_SURFACE_ID":"pane-1","SECRET":"nope"}}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("enqueue gets a reply");
        assert!(reply.contains(r#""queued":true"#), "{reply}");
        let (id, validated) = rx.recv().expect("handler saw the enqueue");
        assert_eq!(id, "e1");
        assert_eq!(validated.agent, "codex");
        assert_eq!(validated.subcommand, "stop");
        assert_eq!(
            validated.environment.get("CMUX_SURFACE_ID").unwrap(),
            "pane-1"
        );
        assert!(validated.environment.get("SECRET").is_none());

        let reply = client::request(
            &name,
            r#"{"id":"u1","method":"zzz","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("unknown method gets a structured reply");
        assert!(reply.contains("unknown_method"), "{reply}");

        let reply = client::request(
            &name,
            r#"{"id":"i1","method":"agent.hook.enqueue","params":{"agent":"codex","subcommand":"NOT VALID"}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("invalid params get a structured reply");
        assert!(reply.contains("invalid_params"), "{reply}");
    }

    #[test]
    fn serves_capabilities_and_routes_app_calls() {
        let name = unique_name("caps-app");
        let (tx, rx) = std::sync::mpsc::channel();
        let handler: HandlerFn = Arc::new(move |call| {
            let IpcCall::App(app) = call else {
                return None;
            };
            if tx.send(app.request.clone()).is_err() {
                return None;
            }
            // One caller asked for a failure to exercise the problem path.
            if app.id.as_str() == Some("e1") {
                return Some(Err(RequestProblem::not_found("workspace 'nope' is gone")));
            }
            Some(Ok(serde_json::json!({ "workspaces": [] })))
        });
        let server_pid = ancestry::server_own_pid();
        let name_for_server = name.clone();
        std::thread::spawn(move || run(name_for_server, false, handler, server_pid));

        // Warmup: static capabilities answer without touching the handler.
        let mut caps = None;
        for _ in 0..25 {
            match client::request(
                &name,
                r#"{"id":"c1","method":"system.capabilities","params":{}}"#,
                Duration::from_millis(250),
                DEFAULT_RESPONSE_BUDGET,
            ) {
                Ok(reply) => {
                    caps = Some(reply);
                    break;
                }
                Err(_) => continue,
            }
        }
        let caps = caps.expect("capabilities answer once the server thread is up");
        assert!(caps.contains(r#""protocol":"cmux-socket""#), "{caps}");
        assert!(caps.contains(r#""system.ping""#), "{caps}");

        // The V2 ping alias serves the same probe shape.
        let reply = client::request(
            &name,
            r#"{"id":"p1","method":"system.ping","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("system.ping answered");
        assert!(reply.contains(r#""pong":true"#), "{reply}");

        // App-plane call: id echo + result envelope; the request is
        // forwarded for the app to execute.
        let reply = client::request(
            &name,
            r#"{"id":"w1","method":"workspace.list","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("workspace.list answered");
        assert!(reply.contains(r#""id":"w1""#), "{reply}");
        assert!(reply.contains(r#""ok":true"#), "{reply}");
        assert!(reply.contains(r#""workspaces":[]"#), "{reply}");
        assert_eq!(
            rx.recv().expect("handler saw the app call"),
            AppRequest::WorkspaceList
        );

        // App-plane problem: structured error envelope.
        let reply = client::request(
            &name,
            r#"{"id":"e1","method":"workspace.current","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("problem path answered");
        assert!(reply.contains(r#""ok":false"#), "{reply}");
        assert!(reply.contains(r#""code":"not_found""#), "{reply}");
        assert_eq!(
            rx.recv().expect("handler saw the problem call"),
            AppRequest::WorkspaceCurrent
        );

        // A validation problem never reaches the handler.
        let reply = client::request(
            &name,
            r#"{"id":"b1","method":"surface.send_text","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("invalid app call answered");
        assert!(reply.contains(r#""code":"invalid_params""#), "{reply}");
        // ...and unknown methods still reject visibly.
        let reply = client::request(
            &name,
            r#"{"id":"u1","method":"zzz","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("unknown method answered");
        assert!(reply.contains(r#""unknown_method""#), "{reply}");
    }

    #[test]
    fn rejects_clients_that_are_not_process_tree_descendants() {
        let name = unique_name("rejects");
        // The server acts as if its owner were the child: our test process
        // is not a descendant of the child, so the ancestry gate must
        // answer `unauthorized_peer` instead of dispatching.
        let mut child = spawn_cmd_sleeping(8);
        let child_pid = child.id();
        let name_for_server = name.clone();
        std::thread::spawn(move || {
            run(name_for_server, true, Arc::new(|_| None), child_pid);
        });
        // `unauthorized_peer` is id-less (`id: null`), by design: the gate
        // runs before protocol parsing.
        let reply = client::request(
            &name,
            r#"{"id":"r1","method":"ping","params":{}}"#,
            Duration::from_millis(1500),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("the gate answers with a structured error");
        assert!(reply.contains("unauthorized_peer"), "{reply}");
        let _ = child.kill();
        let _ = child.wait();
    }

    // ----- M4.2: password-gate serving -----

    fn spawn_mode_server(tag: &str, mode: AuthMode, provider: PasswordProvider) -> String {
        let name = unique_name(tag);
        let name_for_server = name.clone();
        std::thread::spawn(move || {
            let config = ServerConfig {
                pipe_name: name_for_server,
                mode,
                password: provider,
            };
            let handler: HandlerFn = Arc::new(|_| None);
            let security = Arc::new(SharedSecurityDescriptor(
                prepare_security().expect("pipe security descriptor"),
            ));
            run_auth(&config, &handler, ancestry::server_own_pid(), security);
        });
        name
    }

    fn static_provider(password: impl Into<String>) -> PasswordProvider {
        let password: String = password.into();
        Arc::new(move || Some(password.clone()))
    }

    /// Waits for the server thread to create its first pipe instance.
    fn warmup<F: FnMut() -> Option<String>>(probe: &mut F) -> String {
        for _ in 0..25 {
            if let Some(reply) = probe() {
                return reply;
            }
        }
        panic!("the pipe never answered");
    }

    fn ping_reply(connection: &mut client::Connection) -> String {
        connection
            .exchange(
                r#"{"id":"p1","method":"ping","params":{}}"#,
                DEFAULT_RESPONSE_BUDGET,
            )
            .expect("ping answered")
    }

    #[test]
    fn password_mode_refuses_everything_before_a_successful_login() {
        let name = spawn_mode_server("pw-gate", AuthMode::Password, static_provider("secret"));
        // An unauthenticated ping gets the structured auth_required refusal.
        let reply = warmup(&mut || {
            client::request(
                &name,
                r#"{"id":"p1","method":"ping","params":{}}"#,
                Duration::from_millis(250),
                DEFAULT_RESPONSE_BUDGET,
            )
            .ok()
        });
        assert!(reply.contains(r#""code":"auth_required""#), "{reply}");
        assert!(
            reply.contains("Authentication required. Send auth <password> first."),
            "{reply}"
        );
    }

    #[test]
    fn v2_login_serves_commands_and_rotates_the_credential() {
        let current = std::sync::Arc::new(std::sync::Mutex::new("secret".to_owned()));
        let seen = std::sync::Arc::clone(&current);
        let name = spawn_mode_server(
            "pw-rotate",
            AuthMode::Password,
            Arc::new(move || Some(seen.lock().unwrap().clone())),
        );

        // Warm up with the refusal…
        let refusal = warmup(&mut || {
            client::request(
                &name,
                r#"{"id":"p1","method":"ping","params":{}}"#,
                Duration::from_millis(250),
                DEFAULT_RESPONSE_BUDGET,
            )
            .ok()
        });
        assert!(refusal.contains("auth_required"), "{refusal}");

        // …then a single connection: login, command, rotation, refusal by
        // disconnect.
        let mut connection =
            client::Connection::open(&name, Duration::from_millis(1000)).expect("pipe connectable");
        let login = connection
            .exchange(
                r#"{"id":"login","method":"auth.login","params":{"password":"secret"}}"#,
                DEFAULT_RESPONSE_BUDGET,
            )
            .expect("auth.login answered");
        assert!(login.contains(r#""ok":true"#), "{login}");
        assert!(login.contains(r#""authenticated":true"#), "{login}");
        assert!(
            !login.contains("required"),
            "the pre-dispatch login ack carries no `required` field: {login}"
        );
        let pong = ping_reply(&mut connection);
        assert!(pong.contains(r#""pong":true"#), "{pong}");

        // The credential rotates under the connection.
        *current.lock().unwrap() = "rotated".to_owned();
        let outcome = connection.exchange(
            r#"{"id":"p2","method":"ping","params":{}}"#,
            DEFAULT_RESPONSE_BUDGET,
        );
        // The server disconnects without a reply (implicit revocation).
        assert!(
            matches!(
                outcome,
                Err(client::IpcError::Transport(_)) | Err(client::IpcError::Timeout)
            ),
            "a rotated credential must refuse to continue: {outcome:?}"
        );
    }

    #[test]
    fn v2_login_rejects_missing_wrong_and_unconfigured() {
        let unconfigured_name =
            spawn_mode_server("pw-unconf", AuthMode::Password, Arc::new(|| None));
        let reply = warmup(&mut || {
            let mut attempt =
                client::Connection::open(&unconfigured_name, Duration::from_millis(250)).ok()?;
            attempt
                .exchange(
                    r#"{"id":"l1","method":"auth.login","params":{}}"#,
                    DEFAULT_RESPONSE_BUDGET,
                )
                .ok()
        });
        assert!(reply.contains(r#""code":"invalid_params""#), "{reply}");
        assert!(
            reply.contains("auth.login requires params.password"),
            "{reply}"
        );

        // No credential configured: auth_unconfigured.
        let mut unconfigured =
            client::Connection::open(&unconfigured_name, Duration::from_millis(1000))
                .expect("pipe connectable");
        let reply = unconfigured
            .exchange(
                r#"{"id":"l2","method":"auth.login","params":{"password":"x"}}"#,
                DEFAULT_RESPONSE_BUDGET,
            )
            .expect("auth.login answered");
        assert!(reply.contains(r#""code":"auth_unconfigured""#), "{reply}");

        let wrong_name =
            spawn_mode_server("pw-wrong", AuthMode::Password, static_provider("secret"));
        let mut wrong = client::Connection::open(&wrong_name, Duration::from_millis(1000))
            .expect("pipe connectable");
        let reply = wrong
            .exchange(
                r#"{"id":"l3","method":"auth.login","params":{"password":"nope"}}"#,
                DEFAULT_RESPONSE_BUDGET,
            )
            .expect("auth.login answered");
        assert!(reply.contains(r#""code":"auth_failed""#), "{reply}");
        // And the connection stays connected to retry (upstream behavior),
        // but non-login commands remain refused.
        let reply = wrong
            .exchange(
                r#"{"id":"p1","method":"system.ping","params":{}}"#,
                DEFAULT_RESPONSE_BUDGET,
            )
            .expect("refusal answered");
        assert!(reply.contains(r#""code":"auth_required""#), "{reply}");
    }

    #[test]
    fn v1_plain_probe_authenticates_with_upstream_texts() {
        let name = spawn_mode_server("pw-v1", AuthMode::Password, static_provider("secret"));
        warmup(&mut || {
            client::request(
                &name,
                r#"{"id":"p1","method":"ping","params":{}}"#,
                Duration::from_millis(250),
                DEFAULT_RESPONSE_BUDGET,
            )
            .ok()
        });

        let mut connection =
            client::Connection::open(&name, Duration::from_millis(1000)).expect("pipe connectable");
        let reply = connection
            .exchange("auth wrong", DEFAULT_RESPONSE_BUDGET)
            .expect("v1 probe answered");
        assert_eq!(reply, "ERROR: Invalid password");

        let reply = connection
            .exchange("auth", DEFAULT_RESPONSE_BUDGET)
            .expect("v1 probe answered");
        assert_eq!(reply, "ERROR: Missing password. Usage: auth <password>");

        let reply = connection
            .exchange("auth secret", DEFAULT_RESPONSE_BUDGET)
            .expect("v1 probe answered");
        assert_eq!(reply, "OK: Authenticated");

        // Authenticated: the next command on the same connection serves.
        let pong = ping_reply(&mut connection);
        assert!(pong.contains(r#""pong":true"#), "{pong}");
    }

    #[test]
    fn non_password_modes_serve_the_login_ack_and_the_v1_tolerance() {
        let name = spawn_mode_server("auto-ack", AuthMode::Automation, Arc::new(|| None));

        // Envelope auth.login in a non-password mode: the coordinator ack.
        let reply = warmup(&mut || {
            client::request(
                &name,
                r#"{"id":"l1","method":"auth.login","params":{}}"#,
                Duration::from_millis(250),
                DEFAULT_RESPONSE_BUDGET,
            )
            .ok()
        });
        assert!(reply.contains(r#""ok":true"#), "{reply}");
        assert!(reply.contains(r#""authenticated":true"#), "{reply}");
        assert!(reply.contains(r#""required":false"#), "{reply}");

        // Plain v1 probe: upstream's unknown-command failure — the exact
        // string the CLI tolerates with a resolved password.
        let mut connection =
            client::Connection::open(&name, Duration::from_millis(1000)).expect("pipe connectable");
        let reply = connection
            .exchange("auth ignored", DEFAULT_RESPONSE_BUDGET)
            .expect("v1 probe answered");
        assert_eq!(reply, "ERROR: Unknown command 'auth'");

        // And commands keep serving without any login.
        let pong = ping_reply(&mut connection);
        assert!(pong.contains(r#""pong":true"#), "{pong}");
    }

    #[test]
    fn password_mode_does_not_inherit_the_ancestry_gate() {
        // Password mode's admission is same-user only (upstream `.password`
        // checks UID, not ancestry): even with a "foreign" owner pid, a
        // same-user authenticated client serves.
        let mut child = spawn_cmd_sleeping(8);
        let child_pid = child.id();
        let name = unique_name("pw-no-ancestry");
        let name_for_server = name.clone();
        std::thread::spawn(move || {
            let config = ServerConfig {
                pipe_name: name_for_server,
                mode: AuthMode::Password,
                password: static_provider("secret"),
            };
            let handler: HandlerFn = Arc::new(|_| None);
            let security = Arc::new(SharedSecurityDescriptor(
                prepare_security().expect("pipe security descriptor"),
            ));
            run_auth(&config, &handler, child_pid, security);
        });

        // Warm up the pipe with a throwaway login attempt.
        let login = warmup(&mut || {
            client::Connection::open(&name, Duration::from_millis(250))
                .ok()
                .and_then(|mut attempt| {
                    attempt
                        .exchange(
                            r#"{"id":"l1","method":"auth.login","params":{"password":"secret"}}"#,
                            DEFAULT_RESPONSE_BUDGET,
                        )
                        .ok()
                })
        });
        assert!(login.contains(r#""ok":true"#), "{login}");

        // The real connection: login, then a served command despite a
        // "foreign" owner pid.
        let mut connection =
            client::Connection::open(&name, Duration::from_millis(1000)).expect("pipe connectable");
        let login = connection
            .exchange(
                r#"{"id":"l2","method":"auth.login","params":{"password":"secret"}}"#,
                DEFAULT_RESPONSE_BUDGET,
            )
            .expect("login answers");
        assert!(login.contains(r#""ok":true"#), "{login}");
        let pong = connection
            .exchange(
                r#"{"id":"p1","method":"ping","params":{}}"#,
                DEFAULT_RESPONSE_BUDGET,
            )
            .expect("authenticated ping serves despite the foreign pid");
        assert!(pong.contains(r#""pong":true"#), "{pong}");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn client_auth_helper_matches_upstream_cli_semantics() {
        // Non-password server + resolved password: the CLI's `Unknown
        // command 'auth'` tolerance keeps the verb working.
        let automation_name =
            spawn_mode_server("cli-tolerated", AuthMode::Automation, Arc::new(|| None));
        warmup(&mut || {
            client::request(
                &automation_name,
                r#"{"id":"p1","method":"ping","params":{}}"#,
                Duration::from_millis(250),
                DEFAULT_RESPONSE_BUDGET,
            )
            .ok()
        });
        let pong = client::request_with_auth(
            &automation_name,
            Some("auth ignored-in-automation"),
            r#"{"id":"c1","method":"ping","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("tolerated auth keeps the verb working");
        assert!(pong.contains(r#""pong":true"#), "{pong}");

        // Password server + wrong password: a hard, diagnosable failure.
        let password_name =
            spawn_mode_server("cli-refused", AuthMode::Password, static_provider("secret"));
        warmup(&mut || {
            client::request(
                &password_name,
                r#"{"id":"p1","method":"ping","params":{}}"#,
                Duration::from_millis(250),
                DEFAULT_RESPONSE_BUDGET,
            )
            .ok()
        });
        let failure = client::request_with_auth(
            &password_name,
            Some("auth nope"),
            r#"{"id":"c1","method":"ping","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        );
        let failure = failure.expect_err("wrong password must fail the verb");
        assert!(
            failure.to_string().contains("Invalid password"),
            "{failure}"
        );

        // Password server + right password: the verb answers.
        let ok = client::request_with_auth(
            &password_name,
            Some("auth secret"),
            r#"{"id":"c1","method":"ping","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("correct password serves the verb");
        assert!(ok.contains(r#""pong":true"#), "{ok}");
    }

    // ----- M4.4: the concurrent connection pool -----

    /// Upstream's admission contract, locally shaped: a client whose
    /// request parks the handler must not hold the pipe — a second client
    /// opens and is answered while the first is still in flight.
    #[test]
    fn a_parked_request_does_not_hold_the_pipe() {
        let name = unique_name("parked");
        let name_for_server = name.clone();
        spawn_config(
            ServerConfig {
                pipe_name: name_for_server,
                mode: AuthMode::Automation,
                password: default_password_provider(),
            },
            Arc::new(move |call| {
                // The parked request: the app-plane hop replaced by a
                // stall long enough to span the second client's exchange.
                if matches!(call, IpcCall::App(_)) {
                    std::thread::sleep(Duration::from_millis(900));
                    return Some(Ok(serde_json::json!({ "slept": true })));
                }
                Some(Ok(serde_json::json!({ "queued": true })))
            }),
        );
        // Warm the pipe: serve one throwaway ping so every worker
        // instance exists before the parked request claims one.
        let warm = client::request(
            &name,
            r#"{"id":"w1","method":"ping","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("warmup ping serves");
        assert!(warm.contains(r#""pong":true"#), "{warm}");

        // A: the parked request (in flight for ~900 ms).
        let parked_name = name.clone();
        let parked = std::thread::spawn(move || {
            client::request(
                &parked_name,
                r#"{"id":"a1","method":"system.identify","params":{}}"#,
                Duration::from_millis(3000),
                Duration::from_millis(2800),
            )
            .expect("parked request eventually serves")
        });

        std::thread::sleep(Duration::from_millis(120));
        let started = std::time::Instant::now();
        let quick = client::request(
            &name,
            r#"{"id":"b1","method":"ping","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("a second client answers while the first is parked");
        let elapsed = started.elapsed();
        assert!(quick.contains(r#""pong":true"#), "{quick}");
        assert!(
            elapsed < Duration::from_millis(400),
            "the second client waited {elapsed:?} — the first connection is holding the pipe"
        );

        // Still parked: A's reply must carry A's OWN id (no cross-talk).
        let reply = parked.join().expect("parked thread joins");
        assert!(reply.contains(r#""id":"a1""#), "{reply}");
    }

    /// Saturation shape: more in-flight parked requests than instances.
    /// The bounded answer is per-connection: every request still gets
    /// exactly one envelope (none silently hung), the extra clients
    /// exhaust their connect budget instead of seeing a malformed reply.
    #[test]
    fn every_pooled_connection_receives_its_own_reply() {
        let name = unique_name("every");
        let name_for_server = name.clone();
        spawn_config(
            ServerConfig {
                pipe_name: name_for_server,
                mode: AuthMode::Automation,
                password: default_password_provider(),
            },
            Arc::new(move |call| {
                if matches!(call, IpcCall::App(_)) {
                    std::thread::sleep(Duration::from_millis(150));
                }
                Some(Ok(serde_json::json!({ "done": true })))
            }),
        );
        let warm = client::request(
            &name,
            r#"{"id":"w1","method":"ping","params":{}}"#,
            Duration::from_millis(1000),
            DEFAULT_RESPONSE_BUDGET,
        )
        .expect("warmup ping serves");
        assert!(warm.contains(r#""pong":true"#), "{warm}");

        let mut joins = Vec::new();
        for index in 0..4 {
            let name = name.clone();
            joins.push(std::thread::spawn(move || {
                client::request(
                    &name,
                    &format!(r#"{{"id":"m{index}","method":"system.identify","params":{{}}}}"#),
                    Duration::from_millis(2500),
                    Duration::from_millis(2400),
                )
                .expect("each pooled request answers")
            }));
        }
        for (index, join) in joins.into_iter().enumerate() {
            let reply = join.join().expect("thread joins");
            assert!(reply.contains(&format!(r#""id":"m{index}""#)), "{reply}");
        }
    }
}
