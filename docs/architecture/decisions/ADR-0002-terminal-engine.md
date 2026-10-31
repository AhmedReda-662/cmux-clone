# ADR-0002: Terminal emulation engine for the Windows port

**Status:** Accepted (owner, 2026-10-18 — stack verified through M0/M1 and every session since)
**Date:** 2026-10-17

## Context

cmux's macOS app embeds the full `libghostty` (`GhosttyKit.xcframework`):
Ghostty owns VT state **and** the Metal renderer, hosts the AppKit NSView, and
spawns the shell itself (process config carries `command`, `working_directory`,
`env_vars`). That engine is macOS/iOS-bound in its packaged form.

Separately, upstream's Rust multiplexer `cmux-tui` already binds Ghostty's
**VT-state engine only**, `libghostty-vt`, through
`cmux-tui/crates/ghostty-vt-sys` (bindgen over `ghostty/include/ghostty/vt.h`).
Its `build.rs` explicitly gates Windows targets (x86_64/aarch64
`*-windows-msvc`, x86_64 `*-windows-gnu`) and builds a static
`libghostty-vt.a` from the `ghostty` submodule with **Zig 0.16.0**. Public
upstream confirms Windows support for `libghostty-vt` (tests run on Windows;
2604 passed / 53 skipped per ghostty-org/ghostty discussion #12371), while the
full `libghostty` on Windows is described by Ghostty maintainers as
"greenfield" (DLL/CRT issues fixed only in March 2026, commit a1370d9).

The engine's responsibilities for our port (kept deliberately separate from
PTY and rendering): VT parsing, screen/scrollback state, resize/reflow,
selection anchors, input/key encoding incl. Kitty keyboard protocol, OSC
handling (title, bell, notify), kitty graphics state.

## Decision (proposed)

**Primary: `libghostty-vt`** — the exact engine cmux's own Rust stack binds,
officially supported on Windows, MIT-licensed (Ghostty), and behaviorally
closest to the reference product (same vendor, same config/theme semantics to
explore later).

The Windows binding path already exists in the reference tree
(`ghostty-vt-sys` with Windows gates). For the port it must be:

- built from a fetched `ghostty` submodule source (or `-Demit-lib-vt` install)
  since this snapshot cannot init submodules;
- verified locally: `cargo test -p` against the engine plus a ConPTY smoke
  test (the portable-pty issue #6783 lesson: ConPTY emits `\e[6n` cursor
  probes and title OSCs; the emulator must reply — upstream `ghostty-vt`
  exposes `Callbacks{on_pty_write,...}` and cmake notes say `zig build
  test-lib-vt` should pass on Windows).

**Fallback: `alacritty_terminal`** crate (Apache-2.0) — proven on Windows
(NabiTerm ships egui+wgpu+alacritty_terminal+portable-pty ConPTY; Paneflow's
Windows GNU/ARM64 builds default to an Alacritty backend). Same layer
boundary (emulation only), zero Zig.

The engine sits behind one internal trait (`TerminalEmulator`) so the
fallback can be swapped without touching PTY or GUI code.

## Consequences

- Requires Zig 0.16.0 on PATH for the primary path (not yet installed — see
  `docs/development/building-windows.md`).
- Build friction on Windows is documented upstream (uucode cache workaround,
  libghostty-rs #9); the PoC must reproduce and record the working recipe in
  `building-windows.md`.
- Config compatibility: Ghostty-config behavior (themes/fonts) is an
  upstream product feature tied to the full engine's config loader; the VT
  engine alone does not load user config. Theme/config loading becomes a
  port-layer responsibility (tracked in feature-parity).

## Alternatives considered

1. **Full `libghostty` as DLL (wintty-style)**: Windows DLL support is new and
   "greenfield", pulls freetype/harfbuzz/glslang/oniguruma, and its C API is
   "young and unversioned" per wintty's README; wintty (C#/WinUI3, DX12) is
   the reference proof but is not Rust. Kept as an observation, not chosen.
2. **Ghostty internals directly** — not exposed/stable; rejected.
3. **Custom VT state machine (e.g. `vte` crate)** — rejected after upstream
   lessons: corner-case explosion (DECAWM/LCF/CJK width combos documented in
   the cmux-win write-up); licensing parity argument favors an upstream
   engine.
