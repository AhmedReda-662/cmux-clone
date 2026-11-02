//! Diagnostic pin: what does egui_tty's emulator auto-write back into the
//! pty when fed ConPTY's real pane stream?
//!
//! Defect context (port plan 2026-10-19): the app's pane reader wedges ~1 s
//! into a paced session while a standalone portable-pty probe fed the same
//! bytes does not. Unlike the probe, the app's `Terminal` installs an
//! `on_pty_write` hook that answers program queries through `Tty::reply` —
//! bytes the probe never sends. These tests pin, byte-exactly, whether
//! ConPTY's observed stream triggers any such reply: if it does, the app
//! writes pty input the probe does not (a real behavioral difference worth
//! chasing); if it does not, the reply path is exonerated for this stream.

use egui_tty::{Terminal, Tty, TtyStream};
use std::sync::{Arc, Mutex};

/// Every `write`/`reply` payload the terminal widget sent, in call order.
type RecordedBytes = Vec<Vec<u8>>;
/// Every `(cols, rows)` pair the widget pushed through `Tty::resize`.
type RecordedResizes = Vec<(u16, u16)>;

/// Records every interaction the terminal widget initiates with the pty,
/// split by which trait method carried it.
#[derive(Default)]
struct RecordingTty {
    writes: Mutex<RecordedBytes>,
    replies: Mutex<RecordedBytes>,
    resizes: Mutex<RecordedResizes>,
}

impl Tty for RecordingTty {
    fn write(&self, bytes: &[u8]) -> egui_tty::Result<()> {
        self.writes.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }

    fn reply(&self, bytes: &[u8]) -> egui_tty::Result<()> {
        self.replies.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }

    fn resize(&self, cols: u16, rows: u16) -> egui_tty::Result<()> {
        self.resizes.lock().unwrap().push((cols, rows));
        Ok(())
    }
}

/// ConPTY's first chunk after spawn (win32-input-mode + focus reporting).
const CHUNK_INIT: &[u8] = b"\x1b[?9001h\x1b[?1004h";

/// cmd.exe's startup clear + first runner echo + window-title report +
/// cursor show, as one delivery.
const CHUNK_ECHO1: &[u8] =
    b"\x1b[?25l\x1b[2J\x1b[m\x1b[H>>> Return FINAL_OLLAMA_OK\r\n\x1b]0;C:\\WINDOWS\\system32\\cmd.exe\x07\x1b[?25h";

/// The runner's second echo, delivered after the repaint.
const CHUNK_ECHO2: &[u8] = b"FINAL_OLLAMA_OK\r\n";

/// The resize repaint ConPTY emits when the app's first frame resizes the
/// pane to 93x40: cursor hide, size report, home, row 1 re-emitted, 40
/// cleared rows, cursor to 2;1, cursor show.
fn repaint_chunk() -> Vec<u8> {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"\x1b[?25l");
    chunk.extend_from_slice(b"\x1b[8;40;93t");
    chunk.extend_from_slice(b"\x1b[H");
    chunk.extend_from_slice(b">>> Return FINAL_OLLAMA_OK");
    for _ in 0..40 {
        chunk.extend_from_slice(b"\x1b[K\r\n");
    }
    chunk.extend_from_slice(b"\x1b[2;1H");
    chunk.extend_from_slice(b"\x1b[?25h");
    chunk
}

/// Feed chunks through the same processing path the app's frame loop uses
/// (`Terminal::poll` → `emulator.vt_write`), then report what the widget
/// wrote back into the pty.
fn feed(chunks: &[Vec<u8>]) -> (RecordedBytes, RecordedBytes, RecordedResizes) {
    let tty = Arc::new(RecordingTty::default());
    let (tx, rx) = std::sync::mpsc::channel();
    for chunk in chunks {
        tx.send(chunk.clone()).expect("chunk send");
    }
    drop(tx);
    let mut terminal = Terminal::new(TtyStream {
        output: rx,
        tty: tty.clone(),
    })
    .expect("terminal construction");
    while terminal.poll() {}
    let writes = std::mem::take(&mut *tty.writes.lock().unwrap());
    let replies = std::mem::take(&mut *tty.replies.lock().unwrap());
    let resizes = std::mem::take(&mut *tty.resizes.lock().unwrap());
    (writes, replies, resizes)
}

#[test]
fn conpty_startup_stream_triggers_no_emulator_replies() {
    let chunks = vec![
        CHUNK_INIT.to_vec(),
        CHUNK_ECHO1.to_vec(),
        repaint_chunk(),
        CHUNK_ECHO2.to_vec(),
    ];
    let (writes, replies, resizes) = feed(&chunks);
    assert!(
        writes.is_empty(),
        "emulator wrote pty input: {} call(s) {:x?}",
        writes.len(),
        writes
    );
    assert!(
        replies.is_empty(),
        "emulator replied to the stream: {} call(s) {:x?}",
        replies.len(),
        replies
    );
    assert!(
        resizes.is_empty(),
        "emulator drove tty resizes from the stream: {} call(s) {:x?}",
        resizes.len(),
        resizes
    );
}
