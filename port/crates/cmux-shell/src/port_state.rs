//! Design lane 4 (feature-parity row 12, Figma nodes 1:456..1:484): the
//! sidebar's `RUNTIME SERVICES` segment. `PortRow` is the display truth;
//! collecting is pure and unit-tested; the sampler reads two Windows
//! snapshots (ToolHelp processes, `GetExtendedTcpTable` listeners) so the
//! UI thread never does process-table work.
//!
//! Row semantics (honest derivation from the design's two states): a port
//! whose owner sits in a pane's process tree shows `:port label UP`; when
//! that listener closes while its pane still lives, the row stays visible
//! as `STOP` (the design renders a stopped service in the same segment â€”
//! without the latch the STOP state could never exist). A pane (or
//! workspace) that vanishes takes its rows with it.
//!
//! Process honesty: the design's labels (`web-vite`, `postgres`) are
//! illustrative mock names; the port shows the owning process's image base
//! name minus its `.exe` suffix (`node`, `python`). Recorded as the
//! adaptation when the design's product-name pipeline is unavailable.

use std::collections::{BTreeMap, BTreeSet};

/// One display row of the services segment.
///
/// `listening` distinguishes the design's `UP`/`STOP` badge inks; the red
/// `STOP` row may outlive its socket only while the owning pane remains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRow {
    pub port: u16,
    /// Owner process label: image base name minus `.exe`, lowercased.
    pub label: String,
    pub listening: bool,
}

/// One Windows process-table row distilled to what tree walking needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub parent: u32,
    /// Image base name minus `.exe`, lowercased (see [`process_label`]).
    pub label: String,
}

/// One sample's snapshots: pidâ†’process info, portâ†’owning pid. Ports are
/// keys, so one row covers a port listening on both address families.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListenerSample {
    pub processes: BTreeMap<u32, ProcessInfo>,
    pub listeners: BTreeMap<u16, u32>,
}

/// Latched row state between samples, keyed by the pane instance
/// (workspace index + tree-leader pid) so a closed pane prunes its rows
/// even while sibling panes of the same workspace keep theirs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortHistory(pub BTreeMap<(usize, u32, u16), RowState>);

/// The row half [`PortHistory`] stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowState {
    pub label: String,
    pub listening: bool,
}

/// The pure sample result: display rows per workspace plus the history to
/// seed the next sample.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortDigest {
    pub rows: BTreeMap<usize, Vec<PortRow>>,
    pub history: PortHistory,
}

/// Upper bound on one workspace's segment rows: the design shows three in
/// a 1280Ã—1024 mock; the cap bounds the STOP-latch state growth (a pane
/// that listens on many ports keeps only the lowest-numbered ones).
pub const MAX_SERVICE_ROWS: usize = 8;

/// The design's `:3000` port text.
pub fn format_port(port: u16) -> String {
    format!(":{port}")
}

/// The owner label the design's service names stand in for: the image
/// base name minus a case-insensitive `.exe` suffix, lowercased.
pub fn process_label(image: &str) -> String {
    let lowered = image.to_lowercase();
    let stripped = lowered.strip_suffix(".exe").unwrap_or(&lowered);
    stripped.to_owned()
}

/// The tree rooted at `root` per the snapshot's parent map: the root plus
/// every descendant, collected breadth-first. Cycles (pid reuse) break on
/// the visited set; the root itself is never revisited.
fn member_set(root: u32, processes: &BTreeMap<u32, ProcessInfo>) -> BTreeSet<u32> {
    if !processes.contains_key(&root) {
        return BTreeSet::new();
    }
    let mut members = BTreeSet::new();
    let mut frontier = vec![root];
    members.insert(root);
    while let Some(pid) = frontier.pop() {
        for (child_pid, info) in processes {
            if info.parent == pid && members.insert(*child_pid) {
                frontier.push(*child_pid);
            }
        }
    }
    members
}

/// Pure sampleâ†’rows: live listeners owned by each root's tree surface as
/// `UP`; history rows that lost their socket flip `STOP`; history under a
/// vanished pane root is pruned silently. Rows sort by port ascending.
pub fn collect_rows(
    roots: &[(usize, u32)],
    sample: &ListenerSample,
    history: &PortHistory,
) -> PortDigest {
    let mut trees: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    let mut rows: BTreeMap<usize, BTreeMap<u16, PortRow>> = BTreeMap::new();
    let mut next_history = PortHistory::default();

    // Live state first (a pane whose port re-listened is UP again).
    for &(workspace, root) in roots {
        let subset = trees
            .entry(root)
            .or_insert_with(|| member_set(root, &sample.processes));
        for (&port, &owner) in &sample.listeners {
            if !subset.contains(&owner) {
                continue;
            }
            // Membership implies the owner is in the snapshot.
            let label = sample.processes[&owner].label.clone();
            next_history.0.insert(
                (workspace, root, port),
                RowState {
                    label: label.clone(),
                    listening: true,
                },
            );
            rows.entry(workspace).or_default().insert(
                port,
                PortRow {
                    port,
                    label,
                    listening: true,
                },
            );
        }
    }

    // Latched state: roots that are still alive but no longer listen stop;
    // vanished roots drop without a row. A STOP row keeps whatever label
    // it carried when it last lived.
    for (key, state) in &history.0 {
        let (workspace, root, port) = *key;
        let alive = roots.contains(&(workspace, root));
        if !alive || next_history.0.contains_key(key) {
            continue; // vanished root / already live again this tick
        }
        let domain = rows.entry(workspace).or_default();
        if domain.contains_key(&port) {
            continue; // one row per port; keep the live owner
        }
        next_history.0.insert(
            *key,
            RowState {
                label: state.label.clone(),
                listening: false,
            },
        );
        domain.insert(
            port,
            PortRow {
                port,
                label: state.label.clone(),
                listening: false,
            },
        );
    }

    // Cap + flatten in port order (BTreeMap iteration is ascending).
    // History entries beyond the cap prune (bounded state).
    let mut digest = PortDigest::default();
    for (workspace, rows) in rows {
        let kept: Vec<PortRow> = rows.into_values().take(MAX_SERVICE_ROWS).collect();
        let capped: BTreeSet<u16> = kept.iter().map(|row| row.port).collect();
        next_history
            .0
            .retain(|(ws, _, port), _| *ws != workspace || capped.contains(port));
        digest.rows.insert(workspace, kept);
    }
    digest.history = next_history;
    digest
}

#[cfg(windows)]
mod win {
    //! Windows snapshot half; the pure half above never touches it.
    use super::{ListenerSample, ProcessInfo};
    use std::collections::BTreeMap;

    /// `TH32CS_SNAPPROCESS` walk (same call shapes as the pane-job
    /// extension tests).
    fn process_snapshot() -> std::io::Result<BTreeMap<u32, ProcessInfo>> {
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .map_err(|e| std::io::Error::other(format!("ToolHelp snapshot: {e}")))?;
            let mut out = BTreeMap::new();
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
                    let image = String::from_utf16_lossy(&entry.szExeFile[..len]);
                    if entry.th32ProcessID != 0 {
                        out.insert(
                            entry.th32ProcessID,
                            ProcessInfo {
                                parent: entry.th32ParentProcessID,
                                label: super::process_label(&image),
                            },
                        );
                    }
                    if Process32NextW(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = windows::Win32::Foundation::CloseHandle(snap);
            Ok(out)
        }
    }

    /// TCP listener table (v4 + v6): portâ†’owner. The table's row count is
    /// fixed before the copy; a table growing between the size query and
    /// the copy just picks up on the next sample. The numerically smaller
    /// owning pid wins a shared port (deterministic; per-pid listeners on
    /// one port need socket options rare enough to ignore).
    fn listener_snapshot() -> std::io::Result<BTreeMap<u16, u32>> {
        use windows::Win32::NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
            MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
        };
        use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
        unsafe {
            let mut out = BTreeMap::new();
            for (family, is_v6) in [(u32::from(AF_INET.0), false), (u32::from(AF_INET6.0), true)] {
                let mut size = 0u32;
                // First call: no buffer â†’ the required size. ERROR_SUCCESS
                // with a null buffer also reads as "nothing to do".
                let _ = GetExtendedTcpTable(
                    None,
                    &mut size,
                    false,
                    family,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                );
                if size == 0 {
                    continue;
                }
                let mut buffer = vec![0u8; size as usize];
                let code = GetExtendedTcpTable(
                    Some(buffer.as_mut_ptr() as *mut _),
                    &mut size,
                    false,
                    family,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                );
                if code != 0 {
                    // ERROR_INSUFFICIENT_BUFFER here means the table grew;
                    // the following sample retries.
                    continue;
                }
                if is_v6 {
                    let table = &*(buffer.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
                    // The binding types the flexible array as `[ROW; 1]`;
                    // the real count is dwNumEntries, so slice from the
                    // array's own (padding-corrected) address.
                    let first = std::ptr::addr_of!(table.table) as *const MIB_TCP6ROW_OWNER_PID;
                    let rows = std::slice::from_raw_parts(first, table.dwNumEntries as usize);
                    for row in rows {
                        // Network byte order in the low word (MAPI docs):
                        // from_be reverses the wire order.
                        let port = u16::from_be((row.dwLocalPort & 0xFFFF) as u16);
                        out.entry(port)
                            .and_modify(|owner: &mut u32| *owner = (*owner).min(row.dwOwningPid))
                            .or_insert(row.dwOwningPid);
                    }
                } else {
                    let table = &*(buffer.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
                    let first = std::ptr::addr_of!(table.table) as *const MIB_TCPROW_OWNER_PID;
                    let rows = std::slice::from_raw_parts(first, table.dwNumEntries as usize);
                    for row in rows {
                        let port = u16::from_be((row.dwLocalPort & 0xFFFF) as u16);
                        out.entry(port)
                            .and_modify(|owner: &mut u32| *owner = (*owner).min(row.dwOwningPid))
                            .or_insert(row.dwOwningPid);
                    }
                }
            }
            Ok(out)
        }
    }

    /// Both snapshots, or `None` when either fails (a half sample would
    /// stamp live rows `STOP` on a transient API failure â€” never invent
    /// state the OS did not report).
    pub fn take_sample() -> Option<ListenerSample> {
        let processes = process_snapshot().ok()?;
        let listeners = listener_snapshot().ok()?;
        Some(ListenerSample {
            processes,
            listeners,
        })
    }
}

#[cfg(windows)]
pub use win::take_sample;
#[cfg(not(windows))]
pub fn take_sample() -> Option<ListenerSample> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sample(parents: &[(u32, u32, &str)], listeners: &[(u16, u32)]) -> ListenerSample {
        ListenerSample {
            processes: parents
                .iter()
                .map(|&(pid, parent, image)| {
                    (
                        pid,
                        ProcessInfo {
                            parent,
                            label: image.to_owned(),
                        },
                    )
                })
                .collect(),
            listeners: listeners.iter().map(|&(port, pid)| (port, pid)).collect(),
        }
    }

    fn digest(
        roots: &[(usize, u32)],
        parents: &[(u32, u32, &str)],
        listeners: &[(u16, u32)],
        history: &PortHistory,
    ) -> PortDigest {
        collect_rows(roots, &make_sample(parents, listeners), history)
    }

    #[test]
    fn tree_members_surface_their_listeners_up() {
        // Pane root 100: shell 200 (its child) runs python 300, which
        // listens on 18777 â€” three generations deep.
        let digest = digest(
            &[(0, 100)],
            &[(100, 0, "cmux"), (200, 100, "pwsh"), (300, 200, "python")],
            &[(18777, 300), (5432, 42)],
            &PortHistory::default(),
        );
        let rows = &digest.rows[&0];
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0],
            PortRow {
                port: 18777,
                label: "python".into(),
                listening: true
            }
        );
    }

    #[test]
    fn cycle_safe_member_walk() {
        // pid reuse: 5's parent is 6, and 6's parent is 5. The walk must
        // not loop forever and must still count roots reachable normally.
        let digest = digest(
            &[(0, 1)],
            &[(1, 0, "shell"), (5, 6, "cycle-a"), (6, 5, "cycle-b")],
            &[(80, 5)],
            &PortHistory::default(),
        );
        // The cycle never reaches root 1, so its listener is not ours.
        assert!(digest.rows.get(&0).is_none_or(|rows| rows.is_empty()));
    }

    #[test]
    fn foreign_listener_never_surfaces() {
        let digest = digest(
            &[(0, 100)],
            &[(100, 0, "shell")],
            &[(3000, 9999)],
            &PortHistory::default(),
        );
        assert!(digest.rows.get(&0).is_none_or(|rows| rows.is_empty()));
    }

    #[test]
    fn dead_root_leaves_no_history_row() {
        // Root pane gone between samples: the latched STOP row disappears
        // instead of lingering under a workspace that still has other
        // panes (the key contains the pane root).
        let history = PortHistory(
            [(
                (0usize, 100u32, 18777u16),
                RowState {
                    label: "python".into(),
                    listening: false,
                },
            )]
            .into_iter()
            .collect(),
        );
        let digest = digest(&[(0, 200)], &[(200, 0, "shell")], &[], &history);
        assert!(digest.rows.get(&0).is_none_or(|rows| rows.is_empty()));
        assert!(digest.history.0.is_empty());
    }

    #[test]
    fn stopped_listener_latches_then_recovers() {
        let roots = (0, 100);
        let first = digest(
            &[roots],
            &[(100, 0, "shell"), (200, 100, "python")],
            &[(18777, 200)],
            &PortHistory::default(),
        );
        assert!(first.rows[&0][0].listening);

        // The listener dies; the pane lives: STOP with the last label.
        let second = digest(
            &[roots],
            &[(100, 0, "shell"), (200, 100, "python")],
            &[],
            &first.history,
        );
        let row = &second.rows[&0][0];
        assert_eq!(row.port, 18777);
        assert_eq!(row.label, "python");
        assert!(!row.listening);

        // The service comes back: the same row is UP again.
        let third = digest(
            &[roots],
            &[(100, 0, "shell"), (200, 100, "python")],
            &[(18777, 200)],
            &second.history,
        );
        assert!(third.rows[&0][0].listening);
    }

    #[test]
    fn sort_is_port_ascending_across_states() {
        let history = PortHistory(
            [(
                (0usize, 100u32, 80u16),
                RowState {
                    label: "web".into(),
                    listening: false,
                },
            )]
            .into_iter()
            .collect(),
        );
        let digest = digest(
            &[(0, 100)],
            &[(100, 0, "shell"), (300, 100, "api")],
            &[(8080, 300), (3000, 100)],
            &history,
        );
        let ports: Vec<u16> = digest.rows[&0].iter().map(|row| row.port).collect();
        assert_eq!(ports, vec![80, 3000, 8080]);
        assert!(!digest.rows[&0][0].listening);
        assert!(digest.rows[&0][1].listening);
    }

    #[test]
    fn cap_bounds_rows_and_history() {
        // 10 live ports over the cap keep the 8 lowest.
        let listeners = (1..=10).map(|p| (p as u16, 100u32)).collect::<Vec<_>>();
        let roots = [(0usize, 100u32)];
        let sample = make_sample(&[(100, 0, "shell")], &listeners);
        let history = PortHistory::default();
        let first = collect_rows(&roots, &sample, &history);
        let rows = &first.rows[&0];
        assert_eq!(rows.len(), MAX_SERVICE_ROWS);
        assert_eq!(rows[0].port, 1);
        assert_eq!(rows.last().unwrap().port, MAX_SERVICE_ROWS as u16);

        // All of them stop: the latch holds exactly the capped set.
        let second = collect_rows(
            &roots,
            &make_sample(&[(100, 0, "shell")], &[]),
            &first.history,
        );
        assert_eq!(second.rows[&0].len(), MAX_SERVICE_ROWS);
        assert_eq!(second.history.0.len(), MAX_SERVICE_ROWS);
        assert!(second.rows[&0].iter().all(|row| !row.listening));
    }

    #[test]
    fn label_strips_exe_case_insensitively() {
        assert_eq!(process_label("python.exe"), "python");
        assert_eq!(process_label("NODE.EXE"), "node");
        assert_eq!(process_label("sqlservr"), "sqlservr");
    }

    #[test]
    fn port_text_carries_the_colon() {
        assert_eq!(format_port(3000), ":3000");
    }

    #[test]
    fn real_os_end_to_end_up_then_gone() {
        // The real sampler against the real OS: bind a listener, observe
        // it UP under this process's own tree root, flip STOP on close.
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = std_listener.local_addr().unwrap().port();
        let root = std::process::id();
        let mut history = PortHistory::default();
        let mut seen = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Some(sample) = take_sample() {
                let digest = collect_rows(&[(0, root)], &sample, &history);
                if let Some(row) = digest
                    .rows
                    .get(&0)
                    .and_then(|rows| rows.iter().find(|row| row.port == port))
                {
                    seen = Some(row.clone());
                    history = digest.history;
                    break;
                }
                history = digest.history;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let exe_label = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .map(|name| super::process_label(&name))
            .unwrap_or_default();
        assert_eq!(
            seen,
            Some(PortRow {
                port,
                // The test binary's own image name is the honest label;
                // never hardcode a hash-suffixed test artifact name.
                label: exe_label,
                listening: true,
            }),
            "our own freshly bound listener must show UP under our root"
        );

        drop(std_listener);
        let mut stopped = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Some(sample) = take_sample() {
                let digest = collect_rows(&[(0, root)], &sample, &history);
                let row = digest
                    .rows
                    .get(&0)
                    .and_then(|rows| rows.iter().find(|row| row.port == port));
                if row.is_some_and(|row| !row.listening) {
                    stopped = Some(true);
                    break;
                }
                history = digest.history;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(stopped, Some(true), "closed listener never latched STOP");
    }
}
