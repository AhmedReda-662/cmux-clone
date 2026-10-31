# ADR-0003: Pseudo-terminal layer for Windows (ConPTY via `portable-pty`)

**Status:** Accepted (owner, 2026-10-18 — verified through M0/M1 and the M2 slices)
**Date:** 2026-10-17

## Context

The reference `cmux-tui/crates/cmux-pty` defines the PTY/process boundary the
whole upstream Rust stack codes against: re-exported `portable-pty` traits
(`MasterPty`, `Child`, `ChildKiller`, `ExitStatus`, `PtySize`) plus a
cmux-owned `PtyCommand`/`SpawnedPty`/`PtyOpenError` layer. The Unix backend
(`macos.rs`, `cfg(unix)` — falsely named; it also covers Linux) adds
EINTR-hardened `posix_openpt`, fd hygiene, `close_range`/CLOEXEC handling,
cwd-by-descriptor, `SHELL` resolution, and EOF semantics on writer-drop.

On Windows, `cmux-pty` already delegates to `portable-pty`'s
`native_pty_system()`, which is **wezterm's ConPTY implementation**
(`pty/src/win/conpty.rs`: named pipes + `CreatePseudoConsole` under a mutex,
resize via `COORD`, reader/writer via pipe halves). `portable-pty` 0.9.0
(MIT, wezterm; 21M+ downloads) is actively used in wezterm's nightly Windows
builds.

Known ConPTY realities to design for (upstream-documented):
- ConPTY **re-encodes** the console buffer; output arrives verbatim-with
  additions: OSC title changes (`\e]0;...\a`), clear-screen sequences —
  more verbose than POSIX PTYs (wezterm discussion #2459).
- Startup probes like `\e[6n` (cursor position report) must be **answered**
  by the emulator layer or shells hang (wezterm issue #6783; egui_tty solves
  this with an `on_pty_write` reply hook — the same hook pattern
  `ghostty-vt` exposes as `Callbacks.on_pty_write`).

## Decision (proposed)

Use **`portable-pty` 0.9.x as the PTY layer on Windows** (ConPTY backend),
behind the same trait surface the reference's `cmux-pty` exposes, extended
with a Windows backend module only if/where portable-pty proves insufficient.
Port-side process rules mirror the Unix backend where they apply:
- spawn with `CommandBuilder` (program/args/cwd/env), terminals launched per
  pane;
- explicit child lifecycle: `ChildKiller`-based termination, exit-code
  capture, and **job-object-level cleanup verification** for the process tree
  (Windows requires it: unlike POSIX setsid child trees, ConPTY children can
  outlive the master; REA's own Windows capture limitation stems from exactly
  this class of problem, see `rea-findings.md` Q2);
- resize via `MasterPty::resize` (`COORD`), wired to WM_SIZE/dpi events;
- reader/writer via `try_clone_reader`/`take_writer`, driven by an async
  runtime (tokio) with overlapped I/O, not `as_raw_fd` polling (the
  reference's `as_raw_fd` poll sites are Unix-only seams marked in
  `cmux-tui-core/terminal_host_runtime.rs` and must get a Windows strategy).

## Consequences

- The emulator (ADR-0002) and PTY (this ADR) stay independent: the vertical
  slice must demonstrate shell launch + resize + clean termination through
  this layer before the engine work proceeds.
- Process-cleanup correctness on Windows is a first-class test target
  ( Job Objects, `WaitForSingleObject`, orphaned `conhost.exe` checks) —
  covered in the test strategy.

## Verification (2026-10-17, this machine)

- **`portable-pty` pinned to 0.8.1.** On 0.9.0, `Child::wait()` hangs while
  the master's writer handle is held; dropping the writer terminates the
  child with `0xC000013A` (STATUS_CONTROL_C_EXIT). Reproduced stepwise with
  `cmux-pty-port/examples/pty_probe.rs`, resolved by pinning: 0.8.1 returns
  the natural exit code (`code: 0`) with the writer held. This matches the
  upstream regression report (wezterm/wezterm#6783).
- **Writer lifetime rule**, encoded in `cmux-pty-port` docs + tests: the
  writer must live for the whole session; conhost treats the closed input
  pipe as a kill signal (child gets `0xC000013A`).
- **No EOF on child exit**: the ConPTY output pipe stays open; consumers use
  a reader thread keyed on child lifetime (`egui_tty` example pattern),
  verified by the hanging-test episode that reshaped `conpty_smoke.rs`.
- **ConPTY output re-encoding observed** in the slice's raw bytes:
  `\e[?9001h` (win32-input-mode), `\e[?1004h` (focus reporting),
  `\e[2J`, title OSCs, and (on 0.9.0) an `\e[6n` DSR probe at startup.
  The emulation layer must answer probes / tolerate the noise — the chosen
  `egui_tty`/libghostty-vt layer does (README: reply hook for pty probes).

## Alternatives considered

1. **Raw ConPTY via `windows-sys`/`windows` crate** (hand-rolled, like the
   cmux-win project): more control, more risk; wezterm's implementation is
   battle-tested and MIT. Rejected for the slice; revisit only if
   portable-pty blocks a product need.
2. **`portable_pty` in-tree copy (vendored)** — rejected initially; prefer the
   crates.io dependency pinned in the port workspace manifest.

## Amendment (2026-10-20): pane tree containment via job objects — landed

The "job-object-level cleanup verification" this ADR pre-declared (above)
materialized as a hard requirement through the M3.4 defect slice: without
containment, a disproportionately represented failure class appeared — pane
shells' child processes freezing mid-command in ~7/11 rapid force-kill
relaunch cycles (probe lifecycles were 12/12 clean), with reader streams
healthy and children nondeterministically stalled.

**Decision in force.** Every pane shell's process tree is contained in a
per-pane job object at spawn (`cmux-pty-port::PaneJob`, assigned immediately
after `spawn_command`):
- `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: the kernel terminates the tree
  whenever the owning handle dies — including app death by `TerminateProcess`
  or crash, which no user-level kill walk can cover;
- breakaway is not granted, so descendants cannot escape by request;
- orderly pane shutdown keeps its two-stage shape (writer drop → ConPTY
  terminates the shell process; the job then collapses whatever escaped
  that), mirroring upstream's TERM→grace→KILL escalation with the OS doing
  the waiting.
- `PtySession::terminate()`/`tree()` expose the hard-kill stage
  (`TerminateJobObject`) for paths that need it.
- Containment failure degrades *loudly* (warning log, uncontained spawn) —
  the pre-existing behavior — because the child is already running by then.

This is the Windows realization of upstream's container-ownership invariant
(`AgentForkCommandOutputRunner`/`AgentHookDeliveryProcess` spawn every child
subtree into its own child-led process group with an ordered
termination/reaper chain; process groups do not exist on Windows — job
objects are the same primitive at kernel level).

**Verification (this machine, 2026-10-20).** Two live ConPTY integration
tests in `cmux-pty-port/tests/pane_job.rs`: a descendant survives its
leader's process-level termination while the session is alive (containment
is passive), and collapses when the session drops (KILL_ON_JOB_CLOSE);
`terminate()` empties the tree deterministically. Workspace gates green
(fmt, clippy `-D warnings`, 211 tests). Post-fix smoke battery: 12/12 PASS
with zero runner stalls, measured where the pre-fix cadence wedged ~7/11.
