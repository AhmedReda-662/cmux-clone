# ADR-0007: Pane layout engine — ported CmuxPanes split geometry (`cmux-layout`)

**Status:** Accepted (owner, 2026-10-18 — M2.1 landed and owner-verified)
**Date:** 2026-10-17

## Context

Upstream cmux manages split panes through bonsplit (a vendored Swift
submodule). In this workspace snapshot the submodule is empty, and the macOS
app never programs bonsplit directly: it goes through a pure-geometry layer
in `Packages/macOS/CmuxPanes` — equalize/resize divider planning
(`ExternalTreeNode+SplitGeometry`), split-space verdicts
(`ExternalTreeNode+SplitSpace`), spatial ordering (`+SpatialOrder`,
`SpatialPanelOrder`), focus cycling (`PaneCycleNavigator`), and direction
values — each with its own test suite acting as the behavioral spec.

The port needs the same semantics in a Rust core that stays independent of
the UI framework (ADR-0004 keeps pane geometry out of the renderer, and
M2's config/snapshot layer will serialize this tree).

Options considered:
1. **Fetch the bonsplit submodule and bind it** — heavy cross-language
   surface, license not inventoried for Rust binding, and app behavior is
   reachable through the CmuxPanes math already.
2. **Re-derive semantics by observation** — loses the tests-as-spec
   guarantee. Concrete proof from the first port attempt: subtle
   span-count/clamping rules made one helper wrong, and only the ported
   upstream test suite caught it.
3. **Port `CmuxPanes/Geometry` + `PaneCycleNavigator` one-for-one** into a
   dependency-free Rust crate, porting the upstream test suites verbatim.

## Decision (proposed)

**Option 3.** New crate `port/crates/cmux-layout` (std-only, no
dependencies) owning the pane-tree model (`PixelRect`, `Orientation`,
`TreeNode::Pane/Split`), equalize plans (global and split-run), resize
adjustments with the legacy 0.1–0.9 clamp and enclosing-split candidate
order, split-space verdicts (`Fits` / `FitsAfterEqualizingRun` / `NoSpace`),
spatial pane/panel ordering, and wrapping focus cycling. Deliberate
adaptations are documented in the crate docs: split ids are plain `String`s
(no UUID parsing, so upstream's `hadInvalidSplitIds` flag has no analogue —
snapshot decoding will validate ids), orientations are an enum, and the
tree carries no tab metadata (panel ordering receives a `pane -> [panel]`
map like `SpatialPanelOrder`).

License: **GPL-3.0-or-later**, as a derivative/port of the GPL-3.0-or-later
CmuxPanes package; attribution recorded in the crate header. (The port's
other crates declare no license yet; the port as a whole is expected to be
GPL-3.0-or-later — owner confirmations pending, tracked in the port plan.)

## Consequences

- Upstream tests are the regression spec: the SplitGeometry,
  SpatialOrder (+ PaneCycleNavigator) and SplitSpaceVerdict suites pass
  27/27, giving a stable parity hook when tab/split UI lands in M2.
- The apply side (turning `DividerAdjustment` lists into actual
  renderer-side pane geometry) is a separate concern to be designed with
  the workspace/tab model; this crate stays pure.
- If bonsplit semantics surface that the CmuxPanes files do not cover
  (e.g. nested-split insertion rules), fetch-submodule approval may be
  requested then; today's data shows the geometry layer is complete enough
  to plan and evaluate resize/equalize/refusal behavior.
