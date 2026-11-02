# cmux Windows port — Rust workspace

This directory is the **port product** (see the workspace AGENTS.md): a Windows port
of cmux with a Rust core. `../cmux-main/` is the read-only upstream reference; nothing
here derives from modifying it.

Architecture decisions live in `../docs/architecture/decisions/` (ADRs), feature status in
`../docs/architecture/feature-parity.md`. Build prerequisites and verified setup steps:
`../docs/development/building-windows.md`.

## Crates (by milestone)

| Crate | Scope | Status |
| --- | --- | --- |
| `cmux-pty-port` | Process/PTY edge: ConPTY via `portable-pty`, ADR-0003 | M0 — smoke-tested on this machine |
| `cmux-testsupport` | Shared test helpers (stream drains, replay captures) | M0 |
| `cmux-shell` | Desktop binary: eframe + egui_tty on libghostty-vt, ADR-0002/0004 | M1 |
| `cmux-layout` | Pane-tree/split geometry (port of `CmuxPanes/Geometry`) | M2 |
| `cmux-config` | JSONC config store + themes/commands, ADR-0008 | M2/M6 |
| `cmux-workspaces` | Session snapshots (save/restore/repair), ADR-0009/0010 | M2/M6 |
| `cmux-toast` | WinRT toasts (per-user AUMID shortcut), ADR-0011 | M3 |
| `cmux-ipc` | Named-pipe server/client: DACL + ancestry + auth modes + pool, ADR-0005 | M3/M4 |
| `cmux-hooks` | Agent hook installers (codex config.toml, claude settings doc), ADR-0011 | M3 |
| `cmux-cli` | CLI control tool — **binary `cmux.exe`** (hooks ownership rules match that exact name) | M3+ |
| `cmux-webview2` | Browser pane + CDP automation, ADR-0006 | M5 |
| `egui_tty` (vendored patch) | Terminal widget drawing on the Ghostty VT engine — see `crates/egui_tty/VENDORED.md` | M1 |

Milestone status lives in `../docs/architecture/windows-port-plan.md` §5; the
MSI packaging recipe is `installer/` (see
`../docs/development/building-windows.md`).

## Building

```powershell
cd port
. .\scripts\env.ps1     # puts cargo + zig on PATH (see building-windows.md)
cargo build
cargo test
```

The terminal-emulation dependency (`libghostty-vt`) compiles Ghostty's VT engine from
source and therefore needs Zig 0.15.x unless its artifact is cached; see the docs file
for the exact, verified recipe on this machine.
