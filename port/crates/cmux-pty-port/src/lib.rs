//! Process/PTY edge for the cmux Windows port (ADR-0003).
//!
//! Spawns shells inside a ConPTY via wezterm's `portable-pty` and exposes a narrow
//! session API to the rest of the port.
//!
//! # ConPTY rules verified on this machine (see docs/testing and ADR-0003)
//!
//! - The ConPTY **output pipe does not EOF when the child exits**; consumers
//!   must use a reader thread keyed on child lifetime, not read-to-EOF.
//! - The **writer handle must stay alive** for the session's lifetime:
//!   dropping it terminates the child (`0xC000013A`, observed), and since
//!   portable-pty 0.9.0 also leaves `Child::wait()` hanging while held
//!   (upstream regression; this workspace pins 0.8.1 where `wait()` returns
//!   the natural exit code with the writer held).
//! - ConPTY re-encodes console output: it injects win32-input-mode
//!   (`ESC[?9001h`), focus reporting (`ESC[?1004h`), screen clears, and title
//!   OSCs, and (on 0.9.0's conpty) a startup cursor-position probe
//!   (`ESC[6n`) that the emulator must answer.

pub use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};

use portable_pty::{native_pty_system, CommandBuilder};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// Errors from the PTY layer: `portable-pty` reports `anyhow::Error` internally,
/// kept here so callers get the original context.
pub type PtyError = anyhow::Error;

/// A pane shell's whole process tree, owned by a Windows job object.
///
/// This is the port of upstream's container-ownership invariant
/// (`AgentForkCommandOutputRunner`/`AgentHookDeliveryProcess` spawn every child
/// subtree into its own child-led process group and tear it down in ordered
/// stages): on Windows the OS-native container = a job object, and the process
/// group's "children die when the leader dies" semantics are replaced by
/// kernel-enforced membership rather than signal delivery:
///
/// - **`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`**: the tree is terminated by the
///   kernel when the owning party dies *by any means* (normal exit, panic,
///   `TerminateProcess`, crash). No explicit kill walk can miss descendants,
///   no orphaned console clients can outlive the app.
/// - **Creation with no breakaway allowance**: processes cannot escape the
///   container by request (`CREATE_BREAKAWAY_FROM_JOB` fails).
/// - **Ordered cleanup**: the session drops the wrapper after ConPTY closed
///   (writer drop → child terminate), so the normal pane path terminates cmd
///   first and the job only force-handles grandchildren that escaped
///   process-level termination.
///
/// `kill-tree` semantics of the smoke (`taskkill /T /F`) are the *user-level*
/// fallback this replaces.
pub struct PaneJob {
    /// The owning handle. `None` when the tree could not be contained (see
    /// [`PaneJob::spawn_and_contain`] — the pane then degrades to the
    /// previous un-contained behavior rather than failing the spawn).
    handle: Option<HANDLE>,
}

impl PaneJob {
    /// Contain the child (by PID) in a fresh job object.
    ///
    /// Called immediately after `spawn_command` before the shell can spawn
    /// grandchildren. Errors are reported to the caller, which decides
    /// degrade vs. abort (a containment failure is *not* fatal for a pane:
    /// the child already runs, and killing it would break a live shell).
    fn spawn_and_contain(child_pid: Option<u32>) -> anyhow::Result<PaneJob> {
        let Some(child_pid) = child_pid else {
            return Err(anyhow::anyhow!(
                "pane shell pid unknown (portable-pty returned None); cannot contain the tree"
            ));
        };
        // SAFETY: Win32 job-object FFI; each call's contract is the plain
        // documented one (create → set limits → open by pid → assign).
        let handle = unsafe { CreateJobObjectW(None, None) }
            .map_err(|e| anyhow::anyhow!("CreateJobObjectW failed: {e}"))?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|e| {
            unsafe {
                let _ = CloseHandle(handle);
            }
            anyhow::anyhow!("SetInformationJobObject(KILL_ON_JOB_CLOSE) failed: {e}")
        })?;
        let child = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, child_pid) }
            .map_err(|e| {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                anyhow::anyhow!("OpenProcess(pid={child_pid}) failed: {e}")
            })?;
        unsafe { AssignProcessToJobObject(handle, child) }.map_err(|e| {
            unsafe {
                let _ = CloseHandle(child);
            }
            unsafe {
                let _ = CloseHandle(handle);
            }
            anyhow::anyhow!("AssignProcessToJobObject(pid={child_pid}) failed: {e}")
        })?;
        unsafe {
            let _ = CloseHandle(child);
        }
        Ok(PaneJob {
            handle: Some(handle),
        })
    }

    /// Force-terminate every live member of the tree (upstream's hard-kill
    /// stage). No-op when the tree is already empty.
    pub fn terminate(&self) -> anyhow::Result<()> {
        let Some(handle) = self.handle else {
            return Ok(());
        };
        // Force-killed members exit with 1 (unambiguous "we killed it").
        unsafe { TerminateJobObject(handle, 1).map_err(|e| anyhow::anyhow!("{e}")) }
    }

    /// Whether containment is active for this tree.
    pub fn is_contained(&self) -> bool {
        self.handle.is_some()
    }
}

impl Drop for PaneJob {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // KILL_ON_JOB_CLOSE fires when the last handle closes: even a
            // TerminateProcess'd app still drops its kernel handle, so the
            // tree cannot outlive the app without this path running.
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}

// SAFETY: the owning handle is uniquely held by this struct — no other code
// can observe or close it. Kernel calls through a valid handle are
// thread-agnostic; `terminate` only reads the handle (`&self`, no shared
// mutation), so sharing `&PaneJob` across threads is sound.
unsafe impl Send for PaneJob {}
unsafe impl Sync for PaneJob {}

/// A program to run inside a pseudo-terminal.
#[derive(Debug, Clone)]
pub struct PtySpec {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
}

impl PtySpec {
    pub fn new(program: impl Into<String>) -> Self {
        PtySpec {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The program to launch (basename as given).
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The argument vector as built so far.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The last value bound to `key` (a repeat binds wins, matching the
    /// process environment the child actually sees).
    pub fn env_value(&self, key: &str) -> Option<&str> {
        self.env
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// The default shell for a terminal pane on Windows: PowerShell 7 when
    /// installed, otherwise the Windows PowerShell that ships with the OS.
    pub fn windows_default_shell() -> Self {
        if locate_on_path("pwsh.exe") {
            PtySpec::new("pwsh.exe").arg("-NoLogo")
        } else {
            PtySpec::new("powershell.exe").arg("-NoLogo")
        }
    }
}

fn locate_on_path(executable: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(';')
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .any(|dir| dir.join(executable).is_file())
}

/// A running child process inside a pseudo-terminal.
///
/// The master is shared behind a mutex so a UI thread can resize while an
/// input thread writes; the writer should be taken once and held for the
/// session lifetime (ConPTY kills the child when the writer is dropped).
pub struct PtySession {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    /// The shell tree's container. Lives as long as the session, then dies
    /// (KILL_ON_JOB_CLOSE) with the last handle — see [`PaneJob`].
    job: PaneJob,
}

impl PtySession {
    /// Spawn `spec` inside a fresh ConPTY of `size` cells.
    ///
    /// The spawned tree is contained in a job object before return (the
    /// upstream container-ownership invariant ported to Windows). Containment
    /// failure degrades to the previous un-contained spawn — logged loudly,
    /// pane still functional — because the child is already running at that
    /// point and killing it would fail the launch for an orthogonal reason.
    pub fn spawn(spec: &PtySpec, size: PtySize) -> Result<PtySession, PtyError> {
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| e.context("failed to open a pseudo-terminal"))?;

        let mut command = CommandBuilder::new(spec.program.as_str());
        for arg in &spec.args {
            command.arg(arg);
        }
        if let Some(cwd) = &spec.cwd {
            command.cwd(cwd);
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.context(format!("failed to spawn {:?}", spec.program)))?;
        // Dropping the slave end is required for the reader to observe EOF when the
        // child exits (wezterm/portable-pty convention, see egui_tty's example).
        drop(pair.slave);

        // Contain the tree *before* the shell can run its first command: from
        // here on, even a force-killed host cannot leak the tree (see PaneJob).
        let job = match PaneJob::spawn_and_contain(child.process_id()) {
            Ok(job) => job,
            Err(error) => {
                // Degrade, loudly: a shader pane without containment is the
                // status quo ante, but the invariant must not fail silently.
                eprintln!(
                    "cmux-pty-port: WARNING pane tree not contained in a job object: {error:#}"
                );
                PaneJob { handle: None }
            }
        };

        Ok(PtySession {
            master: Arc::new(Mutex::new(pair.master)),
            child,
            job,
        })
    }

    /// A new reader over the child's terminal output (ConPTY's re-encoded stream).
    pub fn take_reader(&self) -> Result<Box<dyn Read + Send>, PtyError> {
        self.master
            .lock()
            .expect("pty master mutex poisoned")
            .try_clone_reader()
    }

    /// Take the writer for the child's keyboard input. Call once, and **keep
    /// the handle alive**: dropping the writer terminates the child.
    pub fn take_writer(&self) -> Result<Box<dyn Write + Send>, PtyError> {
        self.master
            .lock()
            .expect("pty master mutex poisoned")
            .take_writer()
    }

    /// A shareable handle to the master for UI threads (resize).
    pub fn master_handle(&self) -> Arc<Mutex<Box<dyn MasterPty + Send>>> {
        self.master.clone()
    }

    /// Resize the pseudo-terminal (propagates to the foreground child).
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        self.master
            .lock()
            .expect("pty master mutex poisoned")
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
    }

    /// Terminate the child (ConPTY children are not killed by dropping the master).
    pub fn kill(&mut self) -> Result<(), PtyError> {
        self.child.kill().map_err(anyhow::Error::from)
    }

    /// Wait for the child to exit and report its status.
    ///
    /// Requires the pinned `portable-pty` 0.8.x: 0.9.0 hangs here while the
    /// writer handle exists.
    pub fn wait(&mut self) -> Result<ExitStatus, PtyError> {
        self.child.wait().map_err(anyhow::Error::from)
    }

    /// The tree leader's pid (the shell process `spawn_command` created), for
    /// diagnostics and tree-ownership tests.
    pub fn child_pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// The tree container handle owner on this session (see [`PaneJob`]).
    pub fn tree(&self) -> &PaneJob {
        &self.job
    }
}

/// A session split for a long-running host (see `cmux-shell`): UI-facing I/O
/// (`reader`/`writer`/`master`) plus the `child` handle to observe from a
/// dedicated watcher thread. Keep the writer alive for the session's
/// lifetime; once dropped, ConPTY terminates the child.
pub struct PtyParts {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    pub child: Box<dyn Child + Send + Sync>,
    /// The tree container; dies (persuading the kernel to collapse the tree)
    /// with its last owner — the session that holds the pieces alive.
    pub job: PaneJob,
}

impl PtySession {
    /// Split a fresh session into its parts; the reader/writer/master
    /// surfaces are taken exactly once each.
    pub fn into_parts(self) -> Result<PtyParts, PtyError> {
        let reader = self.take_reader()?;
        let writer = self.take_writer()?;
        let master = self.master.clone();
        Ok(PtyParts {
            reader,
            writer,
            master,
            child: self.child,
            job: self.job,
        })
    }
}
