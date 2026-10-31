# ADR-0014: SSH/remote subsystem - Windows subset decision

**Status:** Accepted (owner-approved 2026-10-23) - subset landed 2026-10-23 (amendment 1)
**Date:** 2026-10-23

## Context

M6's SSH/remote item was an evidence spike, not an implementation target:
feature-parity row 20 ("SSH workspaces `cmux ssh`"), criterion already
naming the Windows constraint - no ControlMaster on Win32-OpenSSH. The
spike (`docs/research/ssh-remote-evidence.md`, 2026-10-23) establishes:

- Upstream's full SSH feature is a stack: `cmux ssh` CLI verb with
  relay-token session attach + user ssh-config-aware connection sharing
  (`CLI/cmux.swift` 13651-13954), the `cmux-remote` Rust engine that
  bootstraps the cmux TUI onto POSIX hosts over OpenSSH
  (`cmux-tui/crates/cmux-remote`, staging/verify/lock/reap logic), and
  desktop workspaces embedding remote sessions with per-machine trust
  (`Sources/RemoteTui/`, `AppDelegate+CmuxSSHURL.swift`). Remote tmux
  `-CC` mirroring and mosh are separately Blocked (row 21).
- Windows facts: OpenSSH client ships with Windows 10 1809+/Server 2019+
  (optional feature, off by default); ControlMaster/ControlPath remain
  unsupported on Win32-OpenSSH (PowerShell/Win32-OpenSSH #1328, open
  through 2026; configurable use fails with "getsockname failed: Not a
  socket"); `cmux-remote`'s client lanes are POSIX-gated
  (`#[cfg(unix)]` in bootstrap/http/services).
- Upstream's own degraded path is a plain interactive OpenSSH session
  when its managed bootstrap is disabled or unavailable
  (`CLI/cmux.swift` 13907-13910). The port's pane model does not depend
  on child environments, sidestepping upstream's `CMUX_*` env-drop
  hazard (AppDelegate 5010-5014).

## Decision (proposed)

**Land row 20's Windows subset as a small M6 slice; defer the managed
remote engine.**

1. SSH panes: a workspace pane whose child process is the Windows
   OpenSSH client (`ssh.exe <target>`), launchable via a port `cmux ssh
   <target>` CLI verb and/or a config custom command, titled by target.
   The terminal lane spawns it like any shell child; ConPTY wrapping
   and UTF-8/keyboard lanes apply unchanged.
2. Lifecycle: reuse the shell's child-exit detection. On graceful exit
   the pane closes like other panes; a future reconnect affordance
   (row 20's "basic reconnect policy") stays a post-ADR refinement -
   Windows grants no ControlMaster, so every new session re-authenticates.
3. Deferred, not adopted: the cmux-tui-over-SSH managed engine (bootstrap,
   relay bridge, remote sessions embedding in workspaces). GPL license
   permits reuse, but Windows needs named-pipe substitutions for its
   Unix-socket lanes, proxy API adaptation, and a connection-sharing
   design that does not exist on Windows OpenSSH. Recorded as the
   scale-up path for a future ADR, alongside rows 21 (tmux `-CC`, mosh)
   which stay Blocked.

## Consequences

- New user-visible surface only through existing patterns (child
  process + pane + CLI verb), so no framework or IPC-grammar changes;
  acceptance criteria ride the existing terminal-lane checks (spawn,
  resize, exit, session reader) plus one harness case for the verb.
- Residuals, documented: no connection multiplexing on Windows (auth
  per session), remote hosts limited to POSIX targets (upstream's
  staging is `sh`-based), ssh// -style trust dialogs and org-policy
  gating are not ported in this subset, and pane detection continues to
  use the shell's own tracking (never child env).
- The deferred engine keeps the SSH workspaces parity gap visible
  (row 20 keeps a "subset landed / engine deferred" status), no silent
  parity claims.

## Alternatives considered

1. **Port the managed engine now** - maximum parity, minimum fit:
   platform work with no Windows precedent upstream, high risk against
   M6's remaining accessibility/session-restore slices.
2. **Do nothing (row 20 stays untouched Post-MVP)** - honest but skips
   the trivially feasible part; users lose a cheap, real SSH pane path.
3. **WSL trampoline for multiplexing** (community workaround) - adds a
   WSL requirement to the native product; rejected as a default path.

## Amendment 1 - subset landed (2026-10-23)

Owner-approved the same day; the slice is in `port/` uncommitted:

- Wire: `workspace.ssh.open` (APP_METHODS 49) parses upstream's
  `runSSHTui` v2 handoff params (`destination` required, `ssh_options`
  array of strings, `port` 1-65535, `identity_file`/`title`/
  `initial_command`, `focus` defaulting true) and re-validates so
  direct clients get the CLI's refusals; the reply mirrors
  `workspace.create` (`workspace_id` + `surface_id`).
- App: `spawn_workspace_with_spec` (the create path refactored to take
  a spec + title + focus) spawns the `ssh.exe` pane through the
  standard terminal lane; child-exit lifecycle and shell-tracked pane
  identity need no new code. The app-known title (destination or
  `--name`) is authoritative over child-set OSC titles - Windows
  ssh.exe sets its console title to its own image path before any
  connection (live evidence in the lane), which would otherwise mask
  the destination.
- CLI: `cmux ssh` mirrors `parseSSHCommandOptions` for the subset
  flags (`--port`, `--identity`, `--name`, `--ssh-option`,
  `--command`, the `--focus` family, `--` passthrough) with verbatim
  refusal texts, refuses the managed-carrier knobs
  (`--window`/`--transport`/`--session`) with the port's
  "not supported by the port" line, consumes `-A`/`-a` as upstream
  does (forwarding follows ssh config), refuses unknown `--` flags
  everywhere, refuses mixed-TTY clusters, and durably encodes a pure
  `-t`/`-T` run as the effective `RequestTTY` option (upstream's
  `sshOptionsPersistingTTYRequest` state machine).
- Verification: `port/harness/test_ssh_m6.py` 28 checks PASS twice
  (parser refusals; a refused-port connect closes the child, pane,
  and workspace while the app lives; silent-listener holds keep the
  title and ssh children observable; `--name`/`--no-focus` semantics;
  clean WM_CLOSE; ssh children reaped with the app), plus the full
  battery (smoke-cli 49x2, turn PASS, M4 16/16, M4.2 13/13, M4.3
  19/19, M4.4 8/8, m51 99/99 x2) and fmt/clippy/test on the Rust
  change.
- Still deferred (unchanged): the managed engine and relay
  bootstrap, the `auth_required` interactive-login loop, connection
  multiplexing (no ControlMaster on Windows), mosh/tmux (row 21),
  and a reconnect affordance. Restored SSH panes come back as plain
  configured shells: the snapshot persists pane shape and tab
  structure, not targets.
