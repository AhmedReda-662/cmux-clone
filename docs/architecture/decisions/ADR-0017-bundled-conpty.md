# ADR-0017: Bundle Microsoft's ConPTY 1.24 redistributable pair (mouse-reporting pass-through)

**Status:** Accepted — the bundle has shipped with the app since the round-10
mouse work (2026-10-27); decisions recorded retroactively here (2026-11-01) so
the installer commit carries the rationale. Receipts are machine-verified.
**Date:** 2026-10-27 (work) / 2026-11-01 (this record)

## Context

Two ConPTY hosts exist on a typical Windows machine:

1. **The stock host** — `kernel32!CreatePseudoConsole` resolves to the OS's
   in-box `conhost`-based host. It re-encodes the console buffer and, on the
   output side, **swallows a child's private mouse-tracking mode requests**
   (`?1000h`, `?1006h`, ...): the requesting program enables SGR mouse
   reporting, but the emulator behind the PTY never learns of it, so mouse
   reporting is dead even with a fully capable terminal widget.
2. **The redistributable host** — Microsoft ships `conpty.dll` +
   `OpenConsole.exe` (the `Microsoft.Windows.Console.ConPTY` NuGet package,
   source microsoft/terminal, MIT) precisely for apps that
   `LoadLibraryW`/spawn it next to their executable. This host passes the
   child's mouse-mode requests through to the PTY output stream
   (microsoft/terminal PR #9970-class pass-through behavior — verified
   live on this machine, below).

`portable-pty` 0.8.1 (the pinned PTY layer, ADR-0003) prefers a `conpty.dll`
found beside the executable via plain `LoadLibraryW` — so the redistributable
host can be sideloaded without forking the PTY stack: no source changes, just
a file placed in the right directory.

The decision matrix that triggered this ADR is the port's mouse-reporting
feature (egui_tty's SGR encoder, round-10): without the newer host the
feature cannot exist, with it the full press/drag/release battery works.

## Decision

Bundle the official pair and stage it wherever a `cmux-shell.exe` runs:

- **Source of truth for the bytes:** `port/third_party/conpty/`
  (`conpty.dll` 109,920 B + `OpenConsole.exe` 1,066,296 B, x64, unmodified
  from NuGet `Microsoft.Windows.Console.ConPTY` **1.24.260710001**; package
  manifest checked in as `nuspec-ConPTY-1.24.260710001.xml`; provenance,
  layout rule and receipts in `port/third_party/conpty/ORIGIN.md`).
- **Dev builds:** `crates/cmux-shell/build.rs` copies the pair next to
  `cmux-shell.exe` into the cargo profile directory on every build
  (degrades loudly-but-successfully when the pair is absent).
- **Installed:** the MSI ships the pair into `Program Files\cmux` (MSI
  `ProductComponents`, ADR-0016-tagged packaging entry in windows-port-plan
  §7, 2026-11-01) — same-directory rule preserved for the installed layout.
- **License:** MIT; the pair is redistributed unmodified with the required
  notice (`port/installer/NOTICES.txt` also ships inside the MSI). The
  third_party directory is not GPL-adjacent and is Carve-out-exempt in the
  repository's `.gitignore` (explicit `!` negations).

## Verification (this machine, 2026-10-27; receipts mirrored in ORIGIN.md)

- **Image identity:** both files are PE32+ x64 (`machine 8664`); version
  resource `1.24.2607.10001`; file sizes as above; the NuGet manifest
  decodes the exact package identity.
- **Stage-1 pass-through probe** (`port/harness/conpty_mouse_stage1.py`,
  standalone ConPTY + child, probe receipt of 2026-10-27): the stock host
  does not deliver the child's `?1000h/?1006h` requests to the emulator
  side; the sideloaded 1.24 host does (probe tag `conptydll` vs
  `kernel32`).
- **App-level battery** (`port/harness/test_terminal_mouse_r10.py`,
  round-10): **Leg B** (drag ink over the shell pane) PASSED; **Leg A**
  (in-pane TUI mouse records reaching the widget) was **0 records —
  unresolved** at the last recorded run, with the ordering probe fix landed
  (`ping_and_hwnd` + `focus_surface`, the R9 ClientSurface → foreground →
  pin idiom). Leg A is scheduled for re-baseline in the packaging-review
  round; the battery's temporary `R10_NO_RESIZE` branch is removed only
  after that re-run.

## Consequences

- Mouse reporting is a real, demonstrated feature rather than a
  blocked-on-OS one; the price is shipping two Microsoft binaries (~1.18 MB).
- The bundle stamps the app's behavior to ConPTY 1.24's host semantics on
  every machine — an old or new in-box host no longer changes what panes
  see. That is also the fragility trade-off: a future redistributable bump
  must re-verify the round-10 battery.
- The pair is unsigned Microsoft code (as published); the MSI itself is
  unsigned until the owner supplies a certificate — an SmartScreen/
  Authenticode-level caveat recorded for the release, not a functional one.
- Upgrades = swap the two files from a newer NuGet package + re-run the
  stage-1 probe and the battery; no code path changes (the sideload rule is
  portable-pty's, not ours).

## Alternatives considered

1. **Stay on the stock host** — mouse reporting impossible without
   out-of-band INPUT injection hacks to the child (rejected: upstream's
   contract is pass-through encoding, and kernel-side fakes break
   full-screen programs).
2. **Implement a custom ConPTY host layer** (interpose on the codec) —
   rejected as out of scope; the official pair already provides exactly the
   needed behavior and is API-stable.
3. **Compile the host from microsoft/terminal source at build time** —
   rejected for CI cost/complexity; the NuGet pair is the sanctioned
   redistributable artifact precisely for this purpose.
