//! Container-invariant tests for `PaneJob` (upstream's process-group
//! ownership, ported to Windows job objects — ADR-0003): the pane's shell
//! subtree is kernel-owned, survives its leader's termination only while the
//! session is alive, and collapses when the session (or the whole host
//! process) dies. Live ConPTY tests; Windows-only by nature.

#![cfg(windows)]

use cmux_pty_port::{PtySession, PtySize, PtySpec};
use std::time::{Duration, Instant};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// One row of the system process snapshot: (pid, parent pid, image name).
fn process_snapshot() -> anyhow::Result<Vec<(u32, u32, String)>> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| anyhow::anyhow!("CreateToolhelp32Snapshot failed: {e}"))?;
        let mut entries = Vec::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                entries.push((entry.th32ProcessID, entry.th32ParentProcessID, name));
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
        Ok(entries)
    }
}

/// Direct child of `parent_pid` running `image` (case-insensitive suffix
/// match, e.g. "ping.exe").
fn find_direct_child(parent_pid: u32, image: &str) -> Option<u32> {
    process_snapshot()
        .ok()?
        .into_iter()
        .find(|(pid, ppid, name)| {
            *ppid == parent_pid && *pid != 0 && name.eq_ignore_ascii_case(image)
        })
        .map(|(pid, _, _)| pid)
}

/// Poll a condition with a deadline, sleeping between attempts.
fn poll_until(deadline: Duration, step: Duration, condition: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(step);
    }
    condition()
}

fn pane_spec(program_args: &[&str]) -> PtySpec {
    let (program, args) = program_args.split_first().expect("non-empty spec");
    let mut spec = PtySpec::new(*program);
    for arg in args {
        spec = spec.arg(*arg);
    }
    spec
}

/// The tree is contained for the session's lifetime, a descendant survives
/// its leader's process-level termination (kernel membership, not cascade),
/// and collapses when the session drops (the process-death stands-in: the
/// handle closes, `KILL_ON_JOB_CLOSE` fires).
#[test]
fn pane_tree_survives_leader_kill_then_collapses_with_session() {
    let spec = pane_spec(&["cmd.exe", "/c", "ping", "-n", "30", "127.0.0.1"]);
    let mut session = PtySession::spawn(&spec, PtySize::default()).expect("spawn pane shell");
    let cmd_pid = session.child_pid().expect("cmd pid known");

    // The shell spawns ping: containment is live but passive — nobody dies
    // at spawn time.
    let ping_pid = poll_until(Duration::from_secs(10), Duration::from_millis(200), || {
        find_direct_child(cmd_pid, "ping.exe").is_some()
    })
    .then(|| find_direct_child(cmd_pid, "ping.exe").expect("ping descendant just seen"))
    .expect("ping descendant appeared within 10s");

    // Leader-level termination must NOT cascade to the descendant (no job
    // kill fired yet — containment is passive while the session is alive).
    session.kill().expect("terminate cmd");
    assert!(
        poll_until(Duration::from_secs(1), Duration::from_millis(100), || {
            process_snapshot()
                .unwrap_or_default()
                .iter()
                .any(|(pid, _, _)| *pid == ping_pid)
        }),
        "descendant unexpectedly died during leader kill (over-aggressive containment?)"
    );

    // Session death = handle close = kernel collapse of the whole tree.
    drop(session);
    assert!(
        poll_until(Duration::from_secs(5), Duration::from_millis(200), || {
            !process_snapshot()
                .unwrap_or_default()
                .iter()
                .any(|(pid, _, _)| *pid == ping_pid)
        }),
        "descendant survived session drop (KILL_ON_JOB_CLOSE did not collapse the tree)"
    );
}

/// `terminate()` is the port's hard-kill stage (upstream's KILL): it empties
/// the tree while the session is still alive, deterministically.
#[test]
fn pane_job_terminate_empties_the_tree() {
    let spec = pane_spec(&["cmd.exe", "/c", "ping", "-n", "60", "127.0.0.1"]);
    let session = PtySession::spawn(&spec, PtySize::default()).expect("spawn pane shell");
    let cmd_pid = session.child_pid().expect("cmd pid known");

    session.tree().terminate().expect("job terminate");

    assert!(
        poll_until(Duration::from_secs(3), Duration::from_millis(200), || {
            !process_snapshot()
                .unwrap_or_default()
                .iter()
                .any(|(pid, _, _)| *pid == cmd_pid)
        }),
        "shell survived TerminateJobObject"
    );
    drop(session);
}
