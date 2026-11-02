# Third-party bundle: Microsoft Windows Console ConPTY redistributable

`conpty.dll` and `OpenConsole.exe` in this directory are Microsoft's official
Windows Terminal ConPTY redistributable pair, redistributed unmodified.

## Origin

- Package: `Microsoft.Windows.Console.ConPTY` version **1.24.260710001**
  (NuGet, published by Microsoft; the authoritative manifest is
  `nuspec-ConPTY-1.24.260710001.xml` in this directory).
- Source project: https://github.com/microsoft/terminal (OpenConsole project;
  MIT License).
- License: **MIT** (per the package nuspec and the terminal project's
  LICENSE file). Redistribution in source or binary form is permitted with
  the license notice; see `port/installer/NOTICES.txt`, which ships with the
  installed MSI.

## Why the port bundles it

The stock (kernel32-conhost) ConPTY host swallows a program's private mouse
tracking requests on the output side so the emulator never sees them; the
1.24 host passes them through (microsoft/terminal PR #9970 behavior).
portable-pty 0.8.1 prefers any `conpty.dll` found next to the executable via
plain `LoadLibraryW`, so the pair is staged into the build's exe directory
and into the installed program directory (see `crates/cmux-shell/build.rs`
and the MSI's ProductComponents).

## Layout rule

The pair must sit next to `cmux-shell.exe` (same directory):

- dev builds: staged by `crates/cmux-shell/build.rs` into `target/<profile>/`
- installed: Program Files\cmux\ (MSI ProductComponents)

## Verification receipts (2026-10-27)

- x64 images (PE header `PE32+`, machine `8664`) for both files.
- File sizes: `conpty.dll` 109,920 bytes; `OpenConsole.exe` 1,066,296 bytes.
- Version resource: 1.24.2607.10001 (`rcedit`/PowerShell
  `Get-Item ...\conpty.dll | % VersionInfo | fl`).
- nuspec decode round-trips the package identity
  `Microsoft.Windows.Console.ConPTY/1.24.260710001`.
- Live receipts: a `?1000h/?1006h` child's requests pass through to the
  emulator (`docs/research/rea-findings.md`, round 10; stage-1 probe
  capture in `port/harness/conpty_mouse_stage1.py` runs).
