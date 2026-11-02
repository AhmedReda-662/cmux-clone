//! Real ConPTY evidence on this machine (test strategy L2). These tests drive
//! actual Windows shells through the `portable-pty` ConPTY backend.
//!
//! Discipline note: output is read through a [`CapturedStream`] because ConPTY
//! does not EOF its output pipe when the child exits; EOF-driven reads hang
//! (verified — that hang is why this file exists in this shape).

use cmux_pty_port::{PtySession, PtySize, PtySpec};
use cmux_testsupport::{wait_for, CapturedStream};
use std::time::Duration;

fn pty_size() -> PtySize {
    PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[test]
fn conpty_echo_smoke() {
    let spec = PtySpec::new("cmd.exe").arg("/c").arg("echo CMUX_M0_PTY_OK");
    let mut session = PtySession::spawn(&spec, pty_size()).expect("spawn cmd.exe under ConPTY");

    let stream = CapturedStream::read_into(session.take_reader().expect("reader"));
    let status = session.wait().expect("wait for child");

    assert!(status.success(), "unexpected exit status {status:?}");
    assert!(
        wait_for(Duration::from_secs(5), || stream
            .text_contains("CMUX_M0_PTY_OK")),
        "echoed marker missing from ConPTY stream; got {:?}",
        String::from_utf8_lossy(&stream.snapshot())
    );
}

#[test]
fn conpty_powershell_reports_exit_code() {
    let spec = PtySpec::new("powershell.exe")
        .arg("-NoLogo")
        .arg("-Command")
        .arg("exit 3");
    let mut session =
        PtySession::spawn(&spec, pty_size()).expect("spawn powershell.exe under ConPTY");

    let stream = CapturedStream::read_into(session.take_reader().expect("reader"));
    let status = session.wait().expect("wait for child");

    // Some output is expected to have flowed while the process ran.
    assert!(
        !stream.snapshot().is_empty(),
        "no output captured from ConPTY"
    );
    assert!(
        !status.success(),
        "expected a failing exit status, got {status:?}"
    );
}
