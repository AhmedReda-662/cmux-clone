# Building on Windows

Verified machine and tool inventory for this workspace, and what can and cannot
run today. Every claim below was checked on the development machine on
2026-10-17; re-verify after tool changes.

## Machine

| Item | Value |
| --- | --- |
| OS | Windows 10 Pro 24H2 (build 26100) — per `HKLM\...\CurrentVersion` |
| Architecture | x64 (primary port target) |
| Repo path | `D:\workspace\Projects\cmux-clone` (not a git repository; contains only `cmux-main/`) |
| MSVC Build Tools | Installed at `C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools` with the `VC.Tools.x86.x64` component (verified via `vswhere`) → the MSVC linker required by Rust's `x86_64-pc-windows-msvc` target is available |

## Installed tools (verified)

| Tool | Version | Notes |
| --- | --- | --- |
| git | 2.56.0.windows.2 | Present; note the workspace root is **not** a git repo and `cmux-main/` has no `.git` |
| Python | 3.14.8 | Invoked as `python` — the `python3` alias is the broken Microsoft Store stub |
| Node.js | 26.11.1 | With npm/npx 12.2.0 |
| Visual Studio Build Tools | 18 | VC++ x64 toolset confirmed |

## Missing tools (verified absent from PATH)

| Tool | Needed for | Consequence today |
| --- | --- | --- |
| bun | `cmux-main` JS/TS checks (`bun run biome:check`, webview builds) | Use `npx biome check` as a fallback for lint checks |

## Installed during M0 (2026-10-17, owner-approved)

| Tool | Version | Command used | Note |
| --- | --- | --- | --- |
| rustup / rustc / cargo | rustc 1.99.1, stable-x86_64-pc-windows-msvc (`minimal` profile) + `rustup component add rustfmt clippy` | official `rustup-init.exe -y --default-toolchain stable-msvc --profile minimal` | PATH set by installer (new shells) — use `port/scripts/env.ps1` in-session |
| Zig | 0.15.2 | zip from `ziglang.org/download/index.json` → `C:\Users\noah\tools\zig-0.15.2\` | **Deliberately on C:** (project is on D:): building Ghostty's VT engine fails when Zig and the project share a drive letter (uucode cache bug, libghostty-rs #9). `env.ps1` prepends it to PATH |

Setup policy: installs happen only with owner approval; this file records the
exact commands and versions used.

`cmux-main/CONTRIBUTING.md` prerequisites (macOS 14+, Xcode 26, Metal toolchain)
apply to building the upstream **macOS app only** — they are not required for
the Windows port and cannot be satisfied on this machine.

## What can run today without installing anything

- `python scripts/verify-local.py` from `cmux-main/` (diff-driven static
  checks; repository code — run only in a trusted checkout).
- Node/npm-based checks in `cmux-main` web trees (via `npx`).
- Reading and analysis of the reference source (the Phase 2 discovery work).

## What cannot run today (and why)

- Any `cargo` command (no Rust toolchain).
- `cmux-tui` builds even with Rust, until Zig 0.16.0 is installed **and** the
  `ghostty` submodule sources are available (the snapshot cannot init
  submodules — no `.git`). Practical path for engine source: fetch
  `manaflow-ai/ghostty` at the pinned commit into the expected path, or use the
  Ghostty upstream repo — decision recorded in the port plan.
- REA Windows PTY process capture (provider limitation — see
  `docs/research/rea-findings.md` Q2).

## Proposed setup (requires approval before running)

1. Rust toolchain via `winget install Rustlang.Rustup` (or the official
   rustup installer), default `stable-x86_64-pc-windows-msvc`.
   `cmux-tui/rust-toolchain.toml` pins 1.95.0 for `cmux-tui` crates; the port
   workspace will set its own toolchain file.
2. Zig 0.16.0 via `winget install zig.zig` (only needed for the ghostty-vt
   engine path; the fallback engine avoids it).
3. bun via `winget install Oven-sh.Bun` (only for upstream `cmux-main` JS
   checks; `npx` substitutes otherwise).

None of these were installed during discovery; see the Phase 7 report for the
exact commands executed.
