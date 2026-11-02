//! Stepwise ConPTY probe: prints after every stage so a hang reveals exactly
//! which step blocks. Run with `cargo run --example pty_probe`.

use cmux_pty_port::{PtySession, PtySize, PtySpec};
use std::thread;
use std::time::Duration;

fn main() {
    eprintln!(
        "[probe] start (drop_writer={})",
        std::env::var("PROBE_DROP_WRITER").unwrap_or_default()
    );
    let spec = PtySpec::new("cmd.exe").arg("/c").arg("echo PROBE_OK");
    eprintln!("[probe] openpty+spawn...");
    let mut session = PtySession::spawn(
        &spec,
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
    )
    .expect("spawn under ConPTY");
    eprintln!("[probe] spawned; reader/writer not acquired yet; sleeping 1s");
    thread::sleep(Duration::from_secs(1));
    eprintln!("[probe] taking reader");
    let mut reader = session.take_reader().expect("reader");
    eprintln!("[probe] reader ok; trying a 1s read...");
    let mut buffer = [0u8; 1024];
    let read = reader.read(&mut buffer).expect("read");
    eprintln!(
        "[probe] read returned {read} bytes: {:?}",
        &buffer[..read.min(64)]
    );
    // Keep the writer alive unless explicitly probing the drop behavior.
    let _writer = if std::env::var("PROBE_DROP_WRITER").as_deref() == Ok("1") {
        eprintln!("[probe] taking and DROPPING writer");
        drop(session.take_writer().expect("writer"));
        None
    } else {
        eprintln!("[probe] taking writer (held for session)");
        Some(session.take_writer().expect("writer"))
    };
    eprintln!("[probe] writer ok; resizing to 40x10");
    session.resize(10, 40).expect("resize");
    eprintln!("[probe] resized; calling wait()...");
    let status = session.wait().expect("wait");
    eprintln!("[probe] wait returned: {status:?}");
}
