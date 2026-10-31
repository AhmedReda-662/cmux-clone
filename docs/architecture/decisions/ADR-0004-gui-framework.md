# ADR-0004: GUI framework for the desktop host

**Status:** Accepted (owner, 2026-10-18 — verified in slice M1)
**Date:** 2026-10-17

## Context

All macOS app UI (SwiftUI/AppKit, NSWindow lifecycle, `GhosttyNSView`) is
replaced by the port's Rust GUI. Requirements: multi-window later, vertical
tab sidebar + split panes now, DPI scaling, IME, GPU-accelerated grid
rendering, clean integration with the emulator state (ADR-0002) — the GUI
draws terminal grids, it does not emulate.

Evidence (Windows-proven stacks, 2026):
- **egui/eframe + egui_tty**: egui_tty (MIT) is a terminal widget on
  libghostty-vt with selection/search/paste/mouse/links working; NabiTerm
  ships a Windows terminal multiplexer (tabs/splits/docking, ConPTY, wgpu
  with software fallback) on egui/eframe; egui 0.36-era IME support exists but
  must be verified for CJK.
- **GPUI (Zed)**: `gpui`/`gpui_platform` on crates.io; Windows uses Win32 +
  DirectWrite with no extra features; Paneflow and ZWG Terminal ship
  GPUI + ConPTY + ghostty-vt on Windows x64 MSVC. Pre-1.0 churn risk.
- **wintty**: C#/WinUI3 + full libghostty + DX12 — reference only (not Rust).
- **Tauri/web**: rejected for the core UI: tasking the manifest (no web UI
  assumed), rendering a dense terminal grid in a webview is a known perf
  category risk, and the port's卖点 is native-grade terminals.

## Decision (proposed)

**Phase M1 (slice): egui/eframe + egui_tty + wgpu** — smallest viable proof
of the architecture with the least custom rendering code and a permissive
license chain; keep everything pane-related shaped by our own pane-tree
model (cmux-layout), not egui_dock's tree (dock as adapter only if needed).

**Scale-up path (post-MVP decision): GPUI** if text/DPI/IME quality or
multi-window needs exceed egui's strengths — the emulator/PTY traits stay
identical, so switching is a rendering-adapter change, not an architecture
change.

## Consequences

- M1 must explicitly verify on Windows: DPI scaling on a HiDPI monitor, IME
  composition (CJK), grid rendering perf with a large scrollback, window
  resize coalescing. Failures feed the GPUI re-evaluation instead of a
  silent substitution.
- egui's immediate-mode model vs the reference's retained SwiftUI/AppKit
  layers: workspace/tab state lives in our own state model; egui draws it.

## Alternatives considered

1. **GPUI first** — higher fidelity, higher churn risk; documented scale-up.
2. **Tauri/Electron web UI** — rejected (perf + native integration goals).
3. **winit + wgpu fully custom** (wezterm-style) — rejected for now: we would
   re-implement widget basics that egui_tty already solves for the terminal
   case.
4. **WinUI3/C# hybrid (wintty-style)** — violates Rust-primary rule and adds
   a second language for no gain over GPUI.
