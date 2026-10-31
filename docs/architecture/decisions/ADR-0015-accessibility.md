# ADR-0015: accessibility substrate (AccessKit) - labels now, OS exposure blocked upstream

*Date:* 2026-10-23. Status: Accepted (with a blocked, re-verification-gated
follow-up). Evidence: docs/research/accessibility-evidence.md (sections 1-5)
and its section 6 live-verified outcome.

## Context

Feature-parity row 26 (upstream: VoiceOver/macOS AX everywhere) maps to
Windows UIA. M6 ran the accessibility evidence spike against the port's
actual stack: egui 0.36 (accesskit 0.24 built into core), eframe 0.36 with
`default-features = false, features = ["default_fonts", "glow"]`, egui_tty
0.2 for the terminal grid. The chrome is custom-painted clickable regions
(`ui.allocate_rect` + painter), which are real egui widgets but carry no
`WidgetInfo` label today.

Upstream reference points: egui's own accesskit surface (in-core, non-
optional), eframe's default-on `accesskit` feature wiring the
`accesskit_winit` adapter (`glow_integration.rs`, feature `accesskit`),
egui-winit's `init_accesskit` + `handle_platform_output` update path, and
`Response::widget_info(impl Fn() -> WidgetInfo)` as the labeling API.

## Decision

1. Ship the **`WidgetInfo` labels** for every interactive chrome element now
   (landed in `cmux-shell/src/main.rs`): "New workspace" (+), each workspace
   row (its name, like upstream's tree rows), "Close workspace <name>",
   "Terminal" per terminal pane, "Embedded browser" per browser interceptor.
   They are egui-side metadata, cost nothing measurable, and are the surface
   any future adapter consumes.
2. **Do not ship the eframe `accesskit` feature in the Windows build yet.**
   The spike landed it first (commit), then live-verified the Windows UIA
   path and reverted it - see "Why" below. The label work stays; the feature
   rides until the exposure can be demonstrated.
3. Record a **re-verification canary lane** (`port/harness/
   test_accessibility_uia.py`) that launches the app hermetically, walks the
   UIA tree through the .NET `System.Windows.Automation` client, and flips
   from SKIP to the full assertion set automatically once a provider tree
   becomes reachable. It asserts the window title today and treats
   "childless provider tree" as SKIP (blocked), not failure.

## Why the feature is off (verified 2026-10-23, this rig)

- With the feature on, the subclass installs (`GetPropW` "AccessKitAdapter"
  on the port window = YES) and `WM_GETOBJECT` is answered with nonzero
  LRESULTs (OBJID_CLIENT + `UiaRootObjectId` probes) - the adapter is live.
- egui produced ~990 passes with the tree producer force-enabled from the
  app side (`ctx.enable_accesskit()`; eprintln evidence in the trace) - the
  `accesskit_update` production gate was on the whole time.
- Despite that, every external client (.NET `System.Windows.Automation`
  walker, RawViewWalker, `AutomationElement.FromHandle` pointed at the
  poked provider, and **real Narrator**) saw exactly the top-level window
  element and no child tree, i.e. the placeholder tier
  (`accesskit_windows` adapter.rs 518-528: unnamed `Role::Window`, no
  children).
- Discriminator: AccessKit's own upstream sample
  (`accesskit_windows-0.32.1/examples/hello_world.rs`, vendored into a
  throwaway crate) - whose activation handler returns a full tree with two
  named buttons - shows the identical outcome on this rig with a visible
  window. So the blockage sits in the environment/upstream layer, not in
  the port or eframe usage.
- Upstream defect class matches: egui issue #8671 (open, filed 2026-10-05
  against 0.36.2 and `main`) describes the placeholder staying reachable
  while the real tree never replaces it because activation requests only
  get answered during shown passes; our observations extend that class to
  Windows builds where updates DO flow.

## Consequences

- Row 26 moves from Optional to "labels landed; UIA exposure blocked
  upstream/environment; canary in harness". No parity claim beyond the
  window title is made today.
- egui_tty terminal-buffer content remains unread (verified: no a11y API in
  egui_tty 0.2) - separate residual, not solvable in cmux-shell.
- Re-run the canary after: (a) an eframe release fixing #8671's class, or
  (b) any rig where the same hello_world sample passes its own tree to
  clients. Only then re-enable the feature and mark the lane PASS.
