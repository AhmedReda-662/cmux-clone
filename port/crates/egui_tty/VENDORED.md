# Vendored crate: egui_tty

This is a **vendored fork** of [`egui_tty` 0.2.0](https://crates.io/crates/egui_tty)
(https://github.com/antoineMoPa/egui_tty, author Antoine M-P, MIT license —
`LICENSE` here is upstream's, verbatim).

## Why vendored

`egui_tty` 0.2.0 is the newest release (checked 2026-10-27 against the
crates.io index: only 0.1.0 and 0.2.0 exist) and **it never forwards mouse
events to the child program**. Its `handle_pointer` only drives Ghostty's
text-selection gesture machine, so any TUI that enables mouse reporting
(`DECSET 1000/1002/1003/1006`, e.g. opencode) sees *nothing* on click. The
underlying engine (`libghostty-vt` 0.2.2) ships a complete mouse encoder
(`mouse::Encoder`, `set_options_from_terminal`, `encode_to_vec`) that the
widget simply never calls.

Nothing can be done about this from outside the crate: the `Terminal` keeps
its emulator private, so mouse-mode state and pointer events are out of
reach of the host. Mouse reporting therefore has to be wired *inside* the
widget, which is why the upstream source is vendored rather than patched
from afar.

## Delta from upstream 0.2.0

- `src/terminal.rs`: `handle_pointer` reports press/release/motion of
  primary/secondary/middle (plus wheel as buttons 4/5 in `report_wheel`)
  through the engine's `mouse::Encoder` while the program has mouse tracking
  on; the text-selection gesture machine and link-hover/open only run when
  tracking is off. New `Terminal` fields hold a reusable encoder/event pair.
- All other files are byte-identical to the registry 0.2.0 sources.
- `Cargo.toml`: examples and dev-dependencies dropped (they exist only in
  the upstream repo); `libghostty-vt` requirement pinned to `0.2.2` (what
  the port's lock already resolved).

## How it builds

The port's workspace root `Cargo.toml` carries
`[patch.crates.io] egui_tty = { path = "crates/egui_tty" }`, so
`cmux-shell`'s ordinary `egui_tty = "0.2"` dependency resolves to this
directory. Keep this fork in sync with any future upstream release before
upgrading.

## Attribution

MIT — see `LICENSE`. This vendored copy is the only modified distribution of
egui_tty in the port; upstream code keeps its original headers.
