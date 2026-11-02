//! Client-identity checks for the pipe server (ADR-0005's Windows analog of
//! upstream's `LOCAL_PEERPID` ppid walk): the client must be a process-tree
//! *descendant* of the server. The DACL already restricts to the current
//! user; this second gate stops another session of the same user (for
//! example some unrelated script) from injecting hook events.
//!
//! Walk: `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` per step, reading
//! each process's parent PID. Depth is capped at 128 like upstream's
//! ppid walk (`LOCAL_PEERPID` sysctl, ≤128 levels). PID reuse races are
//! narrower on Windows (unique per-boot IDs), but each step still
//! re-snapshots — no stale handles accumulate.

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// `true` when walking parents from `client_pid` reaches `server_pid`
/// within 128 levels (self counts: a client that *is* the server's child
/// process group leader resolves in one step).
pub fn is_descendant_of(client_pid: u32, server_pid: u32) -> bool {
    if client_pid == server_pid {
        return true;
    }
    let mut current = client_pid;
    for _ in 0..128 {
        match parent_pid_of(current) {
            Some(parent) if parent != 0 => {
                if parent == server_pid {
                    return true;
                }
                // PID 0 = system idle guard; a cycle impossible by the
                // boot-unique ordering, but the cap bounds any oddity.
                current = parent;
            }
            _ => return false,
        }
    }
    false
}

/// The parent PID of one process, read from a fresh snapshot. `None` when
/// the process exited mid-walk (fine: a dead client is not ours).
fn parent_pid_of(pid: u32) -> Option<u32> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut parent = None;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == pid {
                    parent = Some(entry.th32ParentProcessID);
                    break;
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
        parent
    }
}

/// This process's id — the owner the ancestry walk runs toward
/// (`GetCurrentProcessId`).
pub fn server_own_pid() -> u32 {
    unsafe { windows::Win32::System::Threading::GetCurrentProcessId() }
}

/// The parent of this process (the hook's invoker). The enqueue client uses
/// it as the routed `agent_pid` fallback (upstream `agent_pid=${CMUX_<A>_PID:-${PPID:-}}`).
pub fn current_process_parent_pid() -> Option<u32> {
    parent_pid_of(server_own_pid()).filter(|pid| *pid != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real process-tree round trip: spawn a short-lived child and walk to
    /// our own PID; also check the trivial cases.
    #[test]
    fn walks_a_real_client_to_the_server_pid() {
        use std::os::windows::process::CommandExt;
        let server = server_own_pid();
        assert!(is_descendant_of(server, server), "self is a descendant");
        // A child that lives ~2s while we walk.
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/C", "ping", "-n", "3", "127.0.0.1", ">nul"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .expect("spawn test child");
        let client = child.id();
        let reached = is_descendant_of(client, server);
        let _ = child.wait();
        assert!(reached, "child {client} must walk to server {server}");
        // An unrelated-but-real PID: PID 4 = System, never our descendant.
        assert!(!is_descendant_of(4u32, server));
    }
}
