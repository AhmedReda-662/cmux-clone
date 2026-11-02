//! Shared test helpers for the cmux Windows port workspace (test strategy L2/L3):
//! lifecycle-driven drains of stream data and PTY capture fixtures.

use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Read from `reader` until EOF.
///
/// Note: only safe on streams with real EOF semantics (e.g. sockets). ConPTY
/// readers do **not** reliably EOF when the child exits — the ConPTY handle
/// keeps the pipe open — so PTY tests use [`CapturedStream`] instead.
pub fn drain<R: Read>(mut reader: R) -> io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer)?;
    Ok(buffer)
}

/// Continuously pulls bytes from a reader into shared memory.
///
/// The owning thread is detached; it ends at EOF or read error, and the
/// buffering side never blocks. This models the reader-thread discipline the
/// UI thread needs (see egui_tty's `shell` example and docs/testing layer L2).
pub struct CapturedStream {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl CapturedStream {
    pub fn read_into<R>(reader: R) -> CapturedStream
    where
        R: Read + Send + 'static,
    {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sink = buffer.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        sink.lock()
                            .expect("capture stream mutex poisoned")
                            .extend_from_slice(&chunk[..count]);
                    }
                }
            }
        });
        CapturedStream { buffer }
    }

    /// Current bytes.
    pub fn snapshot(&self) -> Vec<u8> {
        self.buffer
            .lock()
            .expect("capture stream mutex poisoned")
            .clone()
    }

    /// Loss-decoding text containment check (ConPTY streams are UTF-8-ish).
    pub fn text_contains(&self, needle: &str) -> bool {
        String::from_utf8_lossy(&self.snapshot()).contains(needle)
    }
}

/// Poll `predicate` until it returns true or `deadline` elapses. Runs the
/// predicate once more after the deadline; callers assert on this result.
pub fn wait_for(deadline: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    predicate()
}
