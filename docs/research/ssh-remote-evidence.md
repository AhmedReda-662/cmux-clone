# SSH/remote evidence spike (M6) - upstream surface vs Windows reality

*Date:* 2026-10-23. Method: source reads in `cmux-main/` (file/line refs
below) + web verification of Windows OpenSSH facts. Classification:
**verified fact** unless marked *inference/hypothesis*. Purpose: ground
feature-parity row 20 and propose the port's subset decision
(ADR-0014). No code changed.

## 1. Upstream SSH/remote surface (verified from source)

### 1.1 The CLI verb `cmux ssh`
- `CLI/cmux.swift` `runSSH` (13651-13681): generates an ephemeral relay
  endpoint per invocation (port 49152-65535, UUID relay id, 32-byte hex
  relay token), parses SSH options, then delegates to `runSSHWithOptions`
  (13868+).
- Config awareness (13887-13946): a 2 s-bounded `ssh -G`-style lookup
  reads the user's own ssh config; its ControlMaster/ControlPath options
  are merged for connection sharing keyed by a route identifier, and a
  cmux control-path is applied only when user config resolved.
- Two launch modes:
  1. **Managed** (13947-13954): bootstraps and attaches the remote cmux
     TUI (`runSSHTui`) unless bootstrap is disabled, the transport is
     not ssh, or the command disables TTY.
  2. **Unmanaged fallback** (13907-13910): when the user's ssh config
     did not resolve and no explicit command is given, upstream falls
     back to a plain interactive OpenSSH session in the terminal - the
     exact mode the port's Windows subset would offer.
- Session management verbs (`CLI/cmux.swift` 16982+): `cmux ssh session
  list|attach|cleanup|end`.
- Cloud VM lane `cmux vm ssh` / `ssh-info` / `ssh-attach` (7928-7964)
  plus `RemoteDaemonManifest` with `goOS/goArch` assets (13615-13634):
  server-side BUSL - out of port scope (parity row 25: NA).

### 1.2 The engine crate `cmux-remote` (Rust, GPL-3.0, port-reusable)
Located `cmux-main/cmux-tui/crates/cmux-remote/`; modules include
`ssh_bootstrap.rs`, `ssh_args.rs`, `bridge.rs`, `daemon.rs`,
`connection.rs`, `mux_*`, `identity.rs`, `provider/`, `workspace/`.
Bootstrap behavior (verified):
- Staged binary upload probe: gzip-capable remote acknowledged via the
  `cmux-upload:gzip` marker (line 21), 256 KiB chunks; upload staging
  dirs use nonces (25-32) and process-wide install locks.
- Bounded lifecycles: 2 s reap timeout for cleanup (24); bootstrap
  timeout config (69).
- Each bootstrap ssh connection sets `ControlMaster=no` deliberately
  (649 comment: otherwise the run could become the shared master).
- Remote side is POSIX: staging/`sh` scripts (623), fish/tcsh shell
  handling in tests (601); OpenSSH 255 = transport failure (519).
- Payload distribution: per-commit immutable native builds fetched from
  a base URL with checksum caching (34-67); proxy handling documented
  as macOS system proxy + PAC (66-67).
- Machine-agent protocol: newline-delimited JSON over one authenticated
  SSH exec channel (`cmux-tui-machine-agent-protocol/src/lib.rs` 1-3).

### 1.3 Desktop-app integration (verified)
- Workspaces carry remote configs:
  `Sources/RemoteTui/Workspace+SSHTui.swift` 8-28
  (`configureSSHTuiConnection`: workspace fields `remoteConfiguration`,
  `remoteDaemonStatus`, `remoteProxyEndpoint`; auto-connect via
  `sshTuiWorkspaceCoordinator`; `respawnSSHTuiSurface` 22+ for remote
  panes including a `tmuxStartCommand` parameter).
- Remote tmux `-CC` mirroring coordinator exists app-side
  (`AppDelegate.swift` 576) - parity row 21 classifies this Blocked
  (tmux/ET are Unix-only).
- Trust and policy: per-machine clipboard-write policy (AppDelegate
  565); `ssh://` URL scheme with confirm dialogs and "device policy can
  disable remote connections" (`AppDelegate+CmuxSSHURL.swift` 37, 615).
- Lifecycle: remote-SSH subprocesses force-stopped on cancellation
  (2318); plain quit only detaches local ssh clients (2590).
- Process identity: plain `ssh` commonly drops the `CMUX_*` environment
  when exec'd from the user's shell, so upstream detects panes via the
  controlling-TTY device binding instead of environment scope
  (AppDelegate 5010-5014). *Directly relevant to the port:* any
  Windows ssh pane holds its identity in the shell's own tracking, not
  child env.

## 2. Windows platform map (verified externally)

- OpenSSH client is included with Windows 10 (build 1809+) and Windows
  Server 2019+, shipped "not installed" - enabled as an optional
  feature; `ssh`, `sshd`, `ssh-keygen` built in (Microsoft Learn,
  OpenSSH for Windows overview, updated 2025-02).
- **ControlMaster/ControlPath are NOT supported on Win32-OpenSSH** and
  remain an open backlog feature (PowerShell/Win32-OpenSSH issue #1328,
  opened 2019-01-23, still open through 2026; symptom when configured:
  `getsockname failed: Not a socket`). Community workaround = WSL
  trampoline (vscode-remote-release #96) - *inference:* not acceptable
  as the native port's default path.
- `cmux-remote` client side is POSIX-gated: `#[cfg(unix)]` throughout
  `ssh_bootstrap.rs` (1100+), `http.rs` (8+), `services.rs` (15+);
  among cmux-tui crates only `chatmux-relay` carries explicit windows
  cfg. *Verified via grep.* Hypothesis (not verified): Windows
  replacements for Unix-socket lanes would be named pipes (e.g. a
  pipe-based agent lane) - the relay bridge internals were not read
  this session.
- tmux `-CC` and mosh stay Blocked (row 21) - Unix-only technologies.

## 3. Mapping to the port (row 20's criterion)

Row 20 already states the Windows criterion: "launch `ssh` into pane;
basic reconnect policy (no ControlMaster)". The spike confirms:
- **Trivially feasible**: a pane whose child process is `ssh.exe`
  (OpenSSH client ships with Windows) - the port's PTY lane spawns
  arbitrary commands already; ssh is just another program; upstream's
  own unmanaged fallback (13907-13910) is exactly this shape.
- **Identity**: the port tracks panes in the shell process (no env
  dependency), so the upstream `CMUX_*` env-drop hazard does not apply.
- **Reconnect**: the shell already observes child exit for pane
  lifecycle; ssh-driven panes reuse it (deadline-free, no ControlMaster
  to reuse, so a dropped session = new auth prompt by design).
- **Deferred (engine)**: upstream's managed remote-TUI engine could be
  reused license-clean (cmux-remote is GPL-3.0) but needs platform work
  far beyond M6: Unix-socket lanes -> named pipes, macOS proxy/PAC ->
  Windows proxy APIs, connection-sharing redesign without
  ControlMaster, plus per-commit payload hosting the port does not
  operate. Recorded as the documented scale-up path, not M6 work.

## 4. Not verified this session (ledger)

- Win32 named-pipe ssh-agent interop (not fetched; not needed for the
  proposed subset).
- Relay bridge protocol internals (`bridge.rs`, `mux_*`) beyond the
  module map.
- Whether remote Windows hosts are targetable by upstream's staging
  scripts (design reads POSIX-only upstream; assumed out of scope).

## 5. Recommendation (input to ADR-0014)

Adopt row 20's Windows subset now as a small M6 slice: an `ssh` pane
preset (config/custom-command/CLI launchable) running the Windows
OpenSSH client directly, exit detection through the existing pane
lifecycle, documented residual = no connection multiplexing on Windows
OpenSSH (auth per session) and POSIX-only remote hosts. Defer the
managed cmux-tui-over-SSH engine to a post-MVP ADR re-evaluation.
