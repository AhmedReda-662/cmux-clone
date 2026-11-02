//! The pipe **client**: exactly what a hook process needs — open, write one
//! JSON line, read one JSON line back, hang up. Open retries absorb the
//! moments when the single server instance is busy or between instances;
//! the response read is a bounded budget (upstream: 0.5 s) enforced via a
//! reader thread so a wedged server cannot stall a hook.
//!
//! On any failure the caller gets an error and decides to fail open — the
//! CLI always answers `{}` on failure, never blocks an agent run.

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

/// Default connect budget before giving up (upstream probe ≈ 0.5 s).
pub const DEFAULT_CONNECT_BUDGET: Duration = Duration::from_millis(500);
/// Default response budget (upstream: 0.5 s).
pub const DEFAULT_RESPONSE_BUDGET: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub enum IpcError {
    /// The pipe never became connectable within the budget.
    Unavailable(String),
    /// Send failed after connecting.
    Transport(String),
    /// No response within the budget (server wedged or slow).
    Timeout,
    /// Response was not a UTF-8 JSON line.
    MalformedReply(String),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::Unavailable(why) => write!(f, "pipe unavailable: {why}"),
            IpcError::Transport(why) => write!(f, "transport failed: {why}"),
            IpcError::Timeout => write!(f, "no response within the budget"),
            IpcError::MalformedReply(why) => write!(f, "malformed reply: {why}"),
        }
    }
}

/// Sends one request line and returns the raw reply line (without `\n`).</think>Adding the multi-line client API to `client.rs`:<tool_call>edit<arg_key>newString</arg_key><arg_value>/// Sends one request line and returns the raw reply line (without `\n`).
pub fn request(
    pipe_name: &str,
    request_line: &str,
    connect_budget: Duration,
    response_budget: Duration,
) -> Result<String, IpcError> {
    let mut connection = Connection::open(pipe_name, connect_budget)?;
    connection.exchange(request_line, response_budget)
}

/// One open pipe connection speaking multiple line exchanges — the M4.2
/// seam for the password handshake: the login probe and the real request
/// share the connection (per-connection auth state), matching upstream's
/// long-lived `SocketClient`. With the M4.4 pool this is also how a client
/// holds its own instance while others hold theirs; fix the stale doc
/// below (a busy open is now only the saturation boundary).
pub struct Connection {
    file: std::fs::File,
}

impl Connection {
    /// Opens with the shared retry loop; a busy single-instance server is
    /// an expected event, not an error.
    pub fn open(pipe_name: &str, budget: Duration) -> Result<Self, IpcError> {
        Ok(Self {
            file: open_with_retries(pipe_name, budget)?,
        })
    }

    /// Writes one line and reads the next reply line bounded by `budget`.
    pub fn exchange(&mut self, line: &str, budget: Duration) -> Result<String, IpcError> {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        self.file
            .write_all(&bytes)
            .and_then(|_| self.file.flush())
            .map_err(|e| IpcError::Transport(format!("write: {e}")))?;
        read_reply_from(&self.file, budget)
    }

    /// Sends the optional login line first and answers the caller's probe:
    /// on a V1 `auth …` line, upstream's CLI treats any `ERROR:` reply as a
    /// failure *except* `Unknown command 'auth'` (the server is not in
    /// password mode — proceed unauthenticated rather than fail the verb).
    /// Returns the auth probe's reply so the caller can diagnose a real
    /// login failure.
    pub fn exchange_with_auth(
        &mut self,
        auth_line: Option<&str>,
        request_line: &str,
        budget: Duration,
    ) -> Result<String, IpcError> {
        if let Some(auth_line) = auth_line {
            let auth_reply = self.exchange(auth_line, budget)?;
            if is_error_prefix(&auth_reply) && !auth_reply.contains("Unknown command 'auth'") {
                return Err(IpcError::Transport(format!("login refused: {auth_reply}")));
            }
        }
        self.exchange(request_line, budget)
    }
}

/// Convenience wrapper: connect, optional auth line, then the request.
pub fn request_with_auth(
    pipe_name: &str,
    auth_line: Option<&str>,
    request_line: &str,
    connect_budget: Duration,
    response_budget: Duration,
) -> Result<String, IpcError> {
    let mut connection = Connection::open(pipe_name, connect_budget)?;
    connection.exchange_with_auth(auth_line, request_line, response_budget)
}

fn is_error_prefix(reply: &str) -> bool {
    reply.starts_with("ERROR:")
}

/// Opens the pipe with small retries: our server serves one instance at a
/// time, so an occupied or between-instances pipe is *expected*; retry until
/// the connect budget runs out (mirrors upstream's short connection loop).
fn open_with_retries(pipe_name: &str, budget: Duration) -> Result<std::fs::File, IpcError> {
    let deadline = Instant::now() + budget;
    loop {
        let error = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name)
        {
            Ok(file) => return Ok(file),
            Err(error) => error,
        };
        if Instant::now() >= deadline {
            return Err(IpcError::Unavailable(format!("{error}")));
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Reads one reply line from `source` (duplicating the handle into a
/// helper thread) bounded by a timeout — a blocking read on the pipe
/// cannot be interrupted from this thread, so the abstraction is: race it,
/// abandon it on timeout.
fn read_reply_from(source: &std::fs::File, budget: Duration) -> Result<String, IpcError> {
    let clone = source
        .try_clone()
        .map_err(|e| IpcError::Transport(format!("handle clone: {e}")))?;
    read_line_with_deadline(clone, budget)
}

fn read_line_with_deadline(file: std::fs::File, budget: Duration) -> Result<String, IpcError> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("cmux-ipc-reply".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(file);
            let mut line = Vec::new();
            let outcome = match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => Err(IpcError::Transport(
                    "connection closed before reply".to_owned(),
                )),
                Ok(_) => {
                    while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                        line.pop();
                    }
                    String::from_utf8(line).map_err(|e| IpcError::MalformedReply(format!("{e}")))
                }
            };
            let _ = tx.send(outcome);
        })
        .map_err(|e| IpcError::Transport(format!("reader thread: {e}")))?;
    match rx.recv_timeout(budget) {
        Ok(outcome) => outcome,
        Err(_) => Err(IpcError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_render_actionably() {
        assert!(IpcError::Unavailable("missing".to_owned())
            .to_string()
            .contains("missing"));
        assert_eq!(
            IpcError::Timeout.to_string(),
            "no response within the budget"
        );
    }
}
