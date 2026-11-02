# Building on Windows

Verified machine and tool inventory for this workspace, and what can and cannot
run today. Every claim below was checked on the development machine on
2026-10-17; re-verify after tool changes.

## Machine

| Item | Value |
| --- | --- |
| OS | Windows 10 Pro 24H2 (build 26100) — per `HKLM\...\CurrentVersion` |
| Architecture | x64 (primary port target) |
| Repo path | `D:\workspace\Projects\cmux-clone` — a **git repository** since 2026-10 (`origin` = github.com/AhmedReda-662/cmux-clone); `cmux-main/` stays an untracked upstream reference checkout |
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
| Zig | 0.15.2 | zip from `ziglang.org/download/index.json` → `C:\Users\noah\tools\zig-0.15.2\` | **Deliberately on C:** (project is on D:): building Ghostty's VT engine fails when Zig and the project share a drive letter (uucode cache bug, libghostty-rs #9). `env.ps1` prepends it to PATH. CI must mirror this: unpack zig under **C:\** on the runner |

Setup policy: installs happen only with owner approval; this file records the
exact commands and versions used.

## Installer toolchain (added 2026-11-01, owner-approved portable install)

| Tool | Version | Command used | Note |
| --- | --- | --- | --- |
| WiX Toolset (candle/light) | 3.14.1.8722 | `curl.exe -L -o C:\Users\noah\tools\wix314-binaries.zip https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip`, then `Expand-Archive -DestinationPath C:\Users\noah\tools\wix-3.14` | **Portable, no machine-wide install** (owner's choice). Scripts prepend the directory to PATH; nothing was written to the registry or Program Files |

### Building the MSI

```
# from port/
. .\scripts\env.ps1          # cargo + zig on PATH (release lane needs zig)
cargo build --release
installer\build-installer.ps1                       # → target\installer\cmux-installer.msi
installer\validate-installer.ps1 -MsiPath target\installer\cmux-installer.msi
```

Verified receipts (2026-11-01): 5.43 MB MSI, zero ICE/compile errors;
`validate-installer.ps1` admin-extraction shows all six payload files
(`cmux\cmux-shell.exe`, `cmux\cmux.exe`, `cmux\conpty.dll`,
`cmux\OpenConsole.exe`, `cmux\cmux.json.template`, `cmux\NOTICES.txt`).

Gotchas learned, in case the recipe is touched:

- The CLI executable is **`cmux.exe`, not `cmux-cli.exe`** — the crate
  declares `[[bin]] name = "cmux"` and the hooks ownership rules match by
  that exact exe name. Installer specs list both names in checks.
- `candle` default architecture is x86 → passing `ProgramFiles64Folder`
  x64 produces **ICE80** validation failures; the build scripts pass
  `-arch x64` and leave component bitness at that default.
- `WixUI_Minimal` already defines the `ARPNOMODIFY` property — declaring
  it again in the product is a LGHT0091 duplicate-symbol link error.
- WiX 3 `candle` rejects `--` inside XML comments (CNDL0104), hyphens in
  Component/File ID attributes (CNDL0014) and a bare `Upgrade` element
  outside `Product` (CNDL0005); version defines must not be declared
  twice with different guards (CNDL1118).
- The helper scripts are **PS 5.1-hostile if they carry non-ASCII glyphs**
  (BOM-less UTF-8 is read as ANSI) or end without a final newline inside a
  string — both build scripts are ASCII-only and newline-terminated now.

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
