//! Pane shell session plumbing: spawning a pane's shell through the pty
//! layer and the reader thread that turns raw ConPTY bytes into the streams
//! the UI consumes. Shared between the `cmux-shell` binary and integration
//! probes so the exact spawn + reader path can be exercised without the
//! GUI (defect investigation, port plan 2026-10-19).

use crate::notifications::NotificationScanner;
use crate::turns::TurnRuntime;
use cmux_pty_port::{ExitStatus, MasterPty, PtyError, PtySession, PtySize, PtySpec};
use egui_tty::{Tty, TtyStream};
use std::io::Write;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// The child's exit status as the watcher reports it (portable-pty 0.8.x
/// reports `ExitStatus` via `io::Error`; normalized here).
pub type WatchedStatus = Result<ExitStatus, PtyError>;

/// The shell's view of one pty, as egui_tty wants it: keystrokes go to the
/// persisted writer (its lifetime is the session lifetime — see crate
/// docs), resizes go through the shared master handle.
pub struct Pty {
    pub writer: Mutex<Box<dyn Write + Send>>,
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Parks the tree container for the session's lifetime: it must outlive
    /// every pane interaction, and its last close is what collapses the tree
    /// even when the app dies by force (see [`cmux_pty_port::PaneJob`]).
    #[allow(dead_code)] // pure lifetime anchor — no API
    job: cmux_pty_port::PaneJob,
}

impl Tty for Pty {
    fn write(&self, bytes: &[u8]) -> egui_tty::Result<()> {
        let mut writer = self.writer.lock().map_err(egui_tty::Error::msg)?;
        writer.write_all(bytes).map_err(egui_tty::Error::msg)?;
        writer.flush().map_err(egui_tty::Error::msg)
    }

    fn resize(&self, cols: u16, rows: u16) -> egui_tty::Result<()> {
        if std::env::var_os("CMUX_DEBUG_READER").is_some() {
            eprintln!("cmux-shell: ui resize {cols}x{rows}");
        }
        let master = self.master.lock().map_err(egui_tty::Error::msg)?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(egui_tty::Error::msg)
    }
}

/// Events from a pane's shell on the reader→UI channel: recognized OSC
/// desktop notifications (M3.1) and prompt-line turn-detection boundary
/// updates (M3.4, ADR-0011 Amendment 6). Both converge on the shared
/// arrival policy in the UI's event drain.
pub enum PaneEvent {
    Notifications(Vec<crate::notifications::OscNotification>),
    Turn(crate::turns::TurnEvent),
}

/// One started shell session, handed to the app plane: the keyed Tty
/// stream, the event channels, and the tree-leader pid lane 4's services
/// sampler walks (see [`start_shell`]).
pub struct ShellHandoff {
    pub stream: TtyStream,
    pub events: mpsc::Receiver<Vec<PaneEvent>>,
    pub status: mpsc::Receiver<WatchedStatus>,
    pub tree_pid: Option<u32>,
}

/// Spawn the shell and hand back the streams a terminal is built from
/// (reader thread → channel, exactly the pattern egui_tty's example
/// prescribes for pty lifetimes), the desktop-notification events the
/// same bytes carry (ADR-0011), the child-status watcher, and the
/// tree-leader pid (None when portable-pty could not report one).
///
/// `wake` is called after every delivery into the channels: an event that
/// arrives while the UI frame loop is idle must wake it, or the
/// frame-loop deadline sweep never observes the pending turn and its
/// delivery is silently lost (defect found 2026-10-19: perfect streams
/// with no turn delivery until the app was brought to the foreground).
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

pub fn start_shell(spec: PtySpec, wake: WakeFn) -> anyhow::Result<ShellHandoff> {
    let session = PtySession::spawn(
        &spec,
        PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        },
    )
    .map_err(|e| anyhow::anyhow!("{e:#}"))?;
    // Read before into_parts() consumes the session.
    let tree_pid = session.child_pid();
    let parts = session.into_parts().map_err(|e| anyhow::anyhow!("{e:#}"))?;

    let (sender, output) = mpsc::channel();
    let (event_sender, events) = mpsc::channel();
    // One thread reads the pty and forwards to the UI thread; it exits on
    // EOF or read error, or when the channel closes (app exit drops the
    // pty, closing ConPTY and breaking the pipe).
    let mut reader = parts.reader;
    let mut scanner = NotificationScanner::new();
    // M3.4: one prompt-line turn detector per built-in agent definition
    // that publishes a prompt configuration (currently Ollama only; the
    // hook path already covers claude/codex — see turns.rs).
    let mut turn_runtimes: Vec<TurnRuntime> = crate::turns::builtin_definitions()
        .iter()
        .map(TurnRuntime::new)
        .collect();
    let debug_reader = std::env::var_os("CMUX_DEBUG_READER").is_some();
    let reader_started = std::time::Instant::now();
    std::thread::spawn(move || {
        let mut buffer = vec![0u8; 8 * 1024];
        loop {
            if debug_reader {
                // Heartbeat: a spawn-stall shows here as a heartbeat that
                // never gets a following chunk (thread alive, blocked in
                // read), vs none at all (thread died before read).
                eprintln!(
                    "cmux-shell: reader awaiting read at {:.3}s",
                    reader_started.elapsed().as_secs_f64()
                );
            }
            let count = match reader.read(&mut buffer) {
                Ok(0) => {
                    // ConPTY's output pipe normally never EOFs while the
                    // master lives (see cmux-pty-port docs), so an EOF is
                    // always diagnostic-worthy: log it unconditionally.
                    eprintln!("cmux-shell: pane reader: stream ended (eof)");
                    break;
                }
                Ok(count) => count,
                Err(error) => {
                    // Stream errors must not silently kill the stream:
                    // report them unconditionally before the thread exits.
                    eprintln!("cmux-shell: reader error: {error:#}");
                    break;
                }
            };
            if debug_reader {
                // Dev tap: log exactly what ConPTY delivers in chunk
                // order (escape codes included) with the arrival timeline
                // as diagnostics for stream-integration issues; terminal
                // input is never echoed (the pane's *output* only, no
                // secrets logged).
                eprintln!(
                    "cmux-shell: reader chunk ({count} bytes at {:.3}s): {:x?}",
                    reader_started.elapsed().as_secs_f64(),
                    &buffer[..count]
                );
            }
            if sender.send(buffer[..count].to_vec()).is_err() {
                break;
            }
            wake();
            // The same bytes are scanned (M3.1): the raw data still goes
            // to the terminal untouched; recognized OSC 9 / OSC 777
            // desktop notifications ride the separate channel, as do
            // prompt-line turn boundaries (M3.4, ADR-0011 Amendment 6).
            let batch = scanner.scan(&buffer[..count]);
            let now = std::time::Instant::now();
            let mut events: Vec<PaneEvent> = Vec::new();
            if !batch.is_empty() {
                events.push(PaneEvent::Notifications(batch));
            }
            for runtime in turn_runtimes.iter_mut() {
                for turn_event in runtime.consume(&buffer[..count], now) {
                    events.push(PaneEvent::Turn(turn_event));
                }
            }
            if !events.is_empty() {
                if event_sender.send(events).is_err() {
                    break;
                }
                wake();
            }
        }
    });

    // Another thread owns the child and reports its exit status. ConPTY
    // gives no EOF on the output pipe, so the reader alone cannot detect
    // exit. (portable-pty 0.8.x reports ExitStatus via io::Error;
    // normalize here.)
    let (status_sender, status_receiver) = mpsc::channel();
    let mut child = parts.child;
    std::thread::spawn(move || {
        let _ = status_sender.send(child.wait().map_err(anyhow::Error::from));
    });

    let tty = Arc::new(Pty {
        writer: Mutex::new(parts.writer),
        master: parts.master,
        job: parts.job,
    });

    Ok(ShellHandoff {
        stream: TtyStream { output, tty },
        events,
        status: status_receiver,
        tree_pid,
    })
}
