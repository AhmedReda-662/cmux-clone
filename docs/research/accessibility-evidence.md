# Accessibility evidence spike (M6) - upstream story vs the port's Windows path

*Date:* 2026-10-23. Method: upstream source reads (`cmux-main/`), docs.rs reads of the
exact crate versions the port builds with, accesskit.dev, and Microsoft/NVDA web facts.
Classification: **verified fact** unless marked *inference/hypothesis*. Purpose: ground
feature-parity row 26 and the M6 accessibility slice. Companion decision record:
ADR-0015.

## 1. Upstream accessibility surface (verified from source)

- SwiftUI accessibility annotation is used pervasively through the app:
  `accessibilityIdentifier` (test/automation hooks, e.g. `AgentInboxView.swift`
  115, 314), `accessibilityLabel`/`accessibilityHint` (Cloud tree, onboarding),
  `accessibilityElement(children: .combine)` (row aggregation,
  `CloudTreeLocalMachineRowContent.swift` 28).
- Imperative AppKit hooking: `AppDelegate.swift` 19653 installs a swizzled
  `cmux_accessibilityAttributeValue(_:)` on `NSApplication` resolving attributes
  through an app-managed `accessibilityWindowCache` (VoiceOver window grouping);
  `NSAccessibility.post` announcements with high priority
  (`CloudTeamPickerPresentation.swift` 97-102).
- Behavior adaptivity: `NSWorkspace.shared.accessibilityDisplayShouldReduceMotion`
  gating decoration animation (`ComputerUseCursorOverlayController.swift` 470),
  and a decorative overlay panel excluded from the accessibility hierarchy via
  `panel.setAccessibilityElement(false)` (576-578).
- The Ghostty engine's own macOS accessibility machinery is not part of the
  port's stack (the port renders with egui + `egui_tty`), so the Windows port
  cannot inherit it.

## 2. The port's stack today (verified from the port's manifests and source)

- egui/eframe 0.36: **accesskit 0.24 is a non-optional dependency of egui core**
  (the accesskit tree is built inside egui), while the OS-level interop lives in
  eframe's **`accesskit` feature** (egui-winit's optional `accesskit_winit ^0.32`).
  The port pulls `eframe = { version = "0.36", default-features = false,
  features = ["default_fonts", "glow"] }` (`port/crates/cmux-shell/Cargo.toml` 19)
  - **the feature is off today: the app exposes no OS accessibility at all.**
- The chrome is custom-painted clickable regions, which are real egui widgets:
  `ui.allocate_rect(..., Sense::click())` produces a `Response` - the sidebar
  "+ New workspace" region, workspace rows, and per-row close buttons
  (`cmux-shell/src/main.rs` sidebar builder); pane regions do the same: the
  egui_tty terminal widget (`render_pane`) and the browser click interceptor
  (`render_pane` browser branch). None attach `WidgetInfo`, so an enabled
  adapter would only see **unlabeled** interactive elements.
- `egui_tty` 0.2 documents no accessibility API; a terminal grid is painted
  pixels. *Verified via the crates.io docs. Result: terminal buffer content
  is invisible to screen readers even with the adapter on* - a residual, not
  solvable inside cmux-shell (inference: needs exposed text in egui_tty or a
  custom accesskit node layer later).
- Toast delivery (`cmux-toast`) is a channel to OS toast banners - no egui
  canvas involvement, no in-app widget to label.
- The native window title is already set (`eframe::run_native("cmux port ...")`
  at `main.rs` window setup; eframe titles the window from the app name).
- API for labeling (verified, egui 0.36 docs): `Response::widget_info(impl
  Fn() -> WidgetInfo)` - "For accessibility. Call after interacting".

## 3. Windows substrate facts (verified externally)

- **AccessKit's Windows adapter implements UI Automation**, "the current
  Windows accessibility API"; the released adapters (macOS NSAccessibility,
  Unix AT-SPI, Windows UIA) are at rough feature parity - enough for
  non-trivial apps including single/multi-line text input controls; no rich
  text/hypertext yet (accesskit.dev/how-it-works, read 2026-10-23).
  Narrator and NVDA both consume UIA (NVDA supports UIA alongside MSAA/IA2;
  NVDA 2026.2 is current, Windows 10+ only).
- *Verified by docs.rs reads:* eframe 0.36's `accesskit` feature is default-on
  for default-features builds (i.e. normal egui apps get UIA on Windows for
  free), and it is the exact piece the port's `default-features = false`
  build currently excludes.
- *Hypothesis (not yet verified):* Narrator/NVDA runtime behavior against the
  port can only be trusted after the live UIA probe lands - the plan below
  uses a programmatic UIA walk (Windows .NET `System.Windows.Automation`
  client) as the acceptance check instead of manual narration claims.

## 4. Slice decided (ADR-0015, forming this spike)

1. Enable eframe's `accesskit` feature in `cmux-shell` (adds accesskit_winit's
   UIA adapter; egui core already carries accesskit).
2. Attach `WidgetInfo` labels to every interactive chrome element:
   "New workspace" (+), each workspace row (its name, as upstream labels tree
   rows by name), "Close workspace <name>", "Terminal" per terminal pane,
   "Embedded browser" per browser interceptor.
3. Acceptance: a new harness lane launches the app with a fixture profile and
   walks the live UIA tree via .NET `System.Windows.Automation`, asserting the
   window is discoverable with title-ui children carrying the expected
   roles/names. Gates as usual (fmt/clippy/test/build), then smoke x2 +
   full battery (shipped-binary change).

## 5. Known residuals after the slice (honest ledger)

- Terminal buffer text is not screen-reader readable (egui_tty limitation).
- No UIA toasts/announcements inside the canvas yet (UIA Notification events
  are a possible later refinement; toasts already ride the OS layer).
- No reduced-motion, high-contrast, or text-scale adaptations yet (upstream
  has reduce-motion; the port's theme is static - design-lane integration).
- egui keyboard focus traversal (Tab between chrome elements) is not
  configured; pane/workspace switching remains chord-driven.

## 6. Live-verification outcome (2026-10-23): exposure blocked upstream/environment

Method: land the slice (eframe `accesskit` feature + labels), then verify
through a real UIA client walk. Result: the exposure **cannot be
demonstrated on this rig, even for accesskit's own upstream sample** - so
the feature was reverted and the decision record (ADR-0015) scopes the
label-only slice plus a re-verification canary.

Verified trail (all reproducible commands/scripts existed during the spike;
each step below is an observation, not an inference):

1. With eframe `accesskit` on, the port app's window carries the AccessKit
   subclass property ("AccessKitAdapter" `GetPropW` = YES) and answers
   manual `WM_GETOBJECT` probes with nonzero LRESULTs for OBJID_CLIENT(-4)
   and `UiaRootObjectId`(-25) (SendMessageTimeoutW probe). The adapter is
   installed and responds in-process.
2. UIA clients see only the top-level window element (win32 fallback tier):
   `.NET System.Windows.Automation` walker (ControlView and RawView, from
   both the enumerated element and the explicit
   `AutomationElement.FromHandle` poke) reports exactly one element
   (name = window title) and no children. Narrator (real screen reader,
   ~10 s session) saw the same tier.
3. The tree producer ran the whole time: an eframe repro with 200 ms forced
   repaint cadence logged ~990 passes, plus app-side
   `ctx.enable_accesskit()` (eprintln in the trace file). No client-visible
   change.
4. Discriminator: forward-port AccessKit's upstream
   `accesskit_windows-0.32.1/examples/hello_world.rs` (activation handler
   returns the full tree: unnamed window root + "Button 1"/"Button 2")
   unchanged into a throwaway crate. With its window visible, the same
   walker + poke + RawView path shows the identical single-element tree.
   The blockage therefore sits below eframe/egui in the adapter or
   environment layer; canonical samples are the reference condition and the
   reference condition fails here.
5. Upstream defect class match (not proven identical): egui issue #8671
   (open, 2026-10-05, against 0.36.2 and main) - activation requests and
   the initial tree request never replace the placeholder; reporter's
   repro is an occluded/minimized macOS window with a ~40-line eframe patch
   proposed. Our Windows builds differ (window visible, passes running)
   while showing the same placeholder-tier outcome, so #8671 is the closest
   tracked upstream defect, not a confirmed identical cause.

State after the spike: eframe features reverted to the M5-verified
`["default_fonts", "glow"]`; `WidgetInfo` labels kept in `cmux-shell`
(no consumer today - egui-side metadata for the future adapter + egui's own
inspection); canary lane `port/harness/test_accessibility_uia.py` added,
asserting the window title and treating "childless provider tree" as SKIP
(blocked) until the placeholder replacement works anywhere this lane can
run. Both outcomes are honest: re-enabling the feature is a no-op for users
until upstream fix lands, and claiming "UIA-aware" would violate the
verification rule.

