# ADR-0006: Embedded browser pane (WebView2 + CDP)

**Status:** Proposed (M5 scope; recorded to close the substrate question)
**Date:** 2026-10-17

## Context

The reference has two browser stacks:

1. **Native pane** (`Packages/macOS/CmuxBrowser`): WKWebView with automation
   via WebKit JS injection (incl. private `_callAsyncJavaScript…` selectors)
   + an isolated REPL runtime; the automation API is the agent-browser port
   (`cmux-main/docs/agent-browser-port-spec.md`, 125 protocol actions:
   snapshot/eval/click/type/fill/press/hover/scroll/get.*/is.*/screenshot/
   find.*/frame.*/dialog/download.wait/console/errors …).
2. **CDP stack** (`cmux-tui/crates/cmux-tui-cdp` + Chromium overlay
   `cmux-browser/`): renders `Page.screencastFrame` into a TUI pane, drives
   CDP input events; registers a loopback DevTools endpoint over a Unix
   control socket.

WKWebView and its private selectors are unportable. WebView2 (Edge, pre-
installed on Windows 10/11) natively exposes **CDP** via
`CallDevToolsProtocolMethod` / `GetDevToolsProtocolEventReceiver`
(`webview2-com` crate, including per-session variants) — i.e. the CDP
half of the reference design is the closer match on Windows.

## Decision (proposed)

- Render browser panes with **WebView2** (`webview2-com`), one user-data
  folder per browser surface (profile parity with the reference's separate
  browser profile).
- Implement the automation API **over CDP**, reusing the protocol design of
  `cmux-tui-cdp`/agent-browser-port (snapshot-from-semantics, ref-table,
  key/mouse plans); scope P0 subset first (snapshot/eval/click/type/fill/
  press/hover/scroll/get.*/is.*/screenshot) as in the reference's staged
  rollout.
- Browser data import: Chromium-family + Firefox formats are identical on
  Windows — port the detection with `%LOCALAPPDATA%` paths; Safari/Arc
  import = not applicable (no Windows builds).

## Consequences

- WebView2 runtime availability is checked at startup (Evergreen runtime;
  developer fallback documented in `building-windows.md`).
- WKWebView-only behaviors with no CDP equivalent must be classified honestly
  in feature-parity (the reference already marks network interception
  infeasible on WKWebView; on CDP it becomes feasible — a behavior *gain*,
  documented when implemented).
- Boundaries obey the manifest: panes validate targets and never execute
  content-supplied commands (page content = data, per REA policy).

## Alternatives considered

1. **Chromium overlay (cmux-browser product) on Windows** — maximum parity
   with the CDP product path, but heavy (custom Chromium builds/licensing
   surface); deferred, revisit as "optional" post-Capstone.
2. **Wry/tauri webview wrappers** — acceptable technically, but the port
   needs direct CDP access (`webview2-com` gives it directly) and is not
   otherwise a Tauri app.
3. **WebKit-based Windows ports (e.g. WinGTK WebKit)** — rejected:
   non-native dependency, weak automation story.

## Amendment 1 (2026-10-21): M5.1 landed — WebView2 panes inside the shell

**Status: landed for the M5.1 slice** (substrate hosting + shell integration +
persistence + IPC verbs + keyboard routing); the automation API over CDP
stays Proposed (M5.2+). Every claim below was verified against the live
`port/harness/test_browser_m51.py` smoke (39/39 PASS on the final binary)
plus the unit suite (255 tests, fmt/clippy `-D warnings` clean), and the six
earlier baselines re-run on the same binary (M4.1 16, M4.2 13, M4.3 19,
M3.4 9, CLI 8, pool 8×2).

### Verified facts (this amendment)

- **Hosting**: `cmux-webview2` grows a managed `BrowserHost` that the frame
  loop owns per browser leaf: `start(spec, parent_hwnd, accel)` requests the
  environment, and `sync_browser_surfaces` ticks every wired host once per
  frame with `TickParams` (bounds in physical px, visible/focused, window
  pos). Children are the Chromium HWND tree parenting under the app window;
  the smoke pins a child rect to the pane rect (±16 px) and the left edge
  around the divider.
- **User data folders**: one per surface id under
  `%LOCALAPPDATA%\cmux\webview2\<pane-id>`. Kept on disk after `WM_CLOSE` —
  documented deliberate residual (no shell-launch parity means no profile
  mutation risk yet).
- **`add_AcceleratorKeyPressed` lives on `ICoreWebView2Controller`** (the
  base controller interface) in webview2-com-sys 0.39.1 bindings — cross-
  checked against Microsoft's WebView2 Win32 reference (the same interface
  the official samples register on). `KeyEventKind`/`VirtualKey` are plain
  in-process arg reads; `SetHandled(true)` suppresses the browser from
  consuming the chord.
- **Chord route**: the paired shell/router design — the crate's `AccelRouter`
  receives matching keys (registered set = VK 0x31..0x39 from the shell),
  marks args handled, queues the VK, and wakes the frame loop
  (`ctx.request_repaint()`); shell `ui()` maps drained VKs through
  `action_for_accel_key` (digits → `SelectWorkspace(index)`) between
  `process_bindings` and `drain_actions`. Verified end-to-end by the smoke's
  ctrl+2 press *while the webview child owns native keyboard focus* — egui
  never sees that input; only the router can deliver it (app stderr shows
  event → captured → drained → `focus -> p2 (workspace 1)`).
- **Frame reconcile regression fixed (root-caused, surgical)**: the
  M2.x-era rule "model focus follows the keyboard" ran every frame and
  snapped model focus back to the last egui-focused terminal widget — a
  browser leaf has no egui widget, so `open_split`'s programmatic browser
  focus was silently reverted within one frame (evidence trail: focus log
  showed `focus -> p3` with identify still reporting p1). The rule now
  follows only an **egui focus transition** between frames (real
  click/drag or widget handoff); browser overlay clicks
  (`render_pane`'s interceptor) report only on the click frame, so they
  remain transitions by construction. Terminal flows are behavior-
  identical (every click-focus is itself a transition).
- **Persistence**: `PaneLayoutSnapshot.browser_url` (wire `browserUrl`,
  skip-serializing-if-none) rides the session snapshot; save writes the
  pane's `start_url`, and **`browser.navigate` now always updates
  `start_url`** so save/restore reopens the live page (the smoke's restart
  section restores and asserts the saved page reloads, identify replaying
  exactly the saved focused pane id from session.json). Restore gives the
  leaf to `new_browser_pane` (no shell spawn); the host joins on the first
  frame's sync.
- **IPC verbs** (`browser.open_split|navigate|back|forward|reload|url.get`
  + facts on `identify`/`surface.list`): error classes per target state —
  `not_found` (unknown surface), `invalid_state` (browser verb on terminal
  / terminal verb on browser), `invalid_params` (missing/empty URL). Back
  at history start is a **success no-op** (upstream semantics).
- **Diagnosability instrumentation (permanent)**: unfiltered
  `accelerator event: kind=… VK=…` (deduped), `accelerator captured:
  VK 0x…`, `cmux-shell: webview chords drained (…)` only when non-empty,
  `cmux-shell: focus -> <pane> (workspace <index>)` per model-focus
  transition, and `ipc serve <id> <method>` per app-plane request
  (`AppRequest::method_name()` in `cmux-ipc`).

### Residuals (recorded, not blocking M5.1)

- **Modifier gate**: the router matches bare VKs, so digits route regardless
  of the held modifier — `shift+2` typed INTO a webview page would reach the
  router as if a chord. Needs a `GetKeyState(VK_CONTROL)`-style gate at
  capture time (M5.2, small).
- **Focus unification**: (a) a click on the terminal that egui *already*
  focuses while a browser leaf holds model focus produces no egui
  transition and therefore does not retake model focus (accepted corner;
  keyboard delivery still works because native focus moves to the main
  window and egui keeps its focused widget); (b) model focus does not track
  native webview GotFocus/LostFocus — clicking INTO a covered webview pane
  after an external programmatic focus leaves model/native divergent.
  M5.2: surrender egui focus when a browser leaf takes model focus, and
  drive model updates from the controller's GotFocus/LostFocus events.
- **Blocking COM calls on the frame thread**: `SetBounds`/`SetIsVisible`/
  `MoveFocus`/`Navigate` are synchronous cross-process calls; a busy browser
  process can hold a frame past the 400 ms reply budget (observed once as a
  transient single-timeout during smoke development; after the reconcile
  fix and smoke hardening no run reproduced it). M5.2 candidate: drive
  aggressive mutations from the tick queue only, measure frame budget with the browser under load.
- **History readback**: back/forward report success after issuing the
  navigation; `canGoBack/canGoForward/live URL/title` surface through
  `url.get` snapshots (this slice), not through history-verb return values.

## Amendment 2 (2026-10-21): M5.2 residual slice landed — chord gate + focus unification

**Status: landed** (modifier gate, focus unification, teardown attribution);
the automation API over CDP stays the M5.2+ headline. Every claim below was
verified on the final binary by the live smoke `port/harness/test_browser_m51.py`
(43 checks PASS) plus the unit suite (260 tests, fmt/clippy `-D warnings`
clean) and the baselines re-run on the same binary (M4.1 16/16, M4.2 13/13,
M4.3 19/19 after one unrelated SendInput flake retry, M4.4 8/8, CLI 8/8,
M3.4 turn runner PASS).

### Verified facts (this amendment)

- **Modifier gate**: a registered key re-enters the app's action stream ONLY
  while Ctrl is actually held — `AccelRouter::routes_as_chord(ctrl_held,
  virtual_key)` is captured through `GetKeyState(VK_CONTROL)` inside the COM
  callback (the callback runs on the UI thread during input processing, so
  the read matches the event being dispatched; an async read would not).
  `shift+digit` therefore types into the page and `alt+digit` stays the
  browser's accelerator (the accelerator contract still fires those events —
  the kind/unfiltered log shows them — but the gate no longer treats them as
  chords). Smoke negatives: a plain digit and an alt+digit pressed while the
  webview child owns the keyboard never switch workspaces; positive:
  ctrl+2 still routes through the identical path.
- **Focus unification, three mechanisms**:
  - *Click claims*: `render_pane` reports explicit pane clicks through the
    new `clicked_now` channel (terminal widget `response.clicked()` and the
    browser click interceptor); the reconcile routes a click before the
    transition rule, so clicking an egui-focused terminal while a browser
    leaf holds model focus DOES retake model focus (the M5.1 accepted corner
    is closed). The decision is the pure, unit-tested
    `focus_reconcile_target` (claim / no-op on the focused pane / real
    transition without a click / standing-focus no-op).
  - *GotFocus routing*: the controller's `add_GotFocus` (webview2-com
    `FocusChangedEventHandler`) feeds the router's focus queue the pane id
    (`BrowserSpec` gained `surface_id`); the shell drains it and updates
    model focus only for leaves of the ACTIVE workspace, only when not
    already focused. `LostFocus` is deliberately not acted on (blur never
    deselects). Observed in app stderr: `accelerator captured: focus ->
    p3 (webview gained native focus)` for programmatic restores, with
    repeat arrivals no-oping silently.
  - *Egui-focus surrender*: while a browser leaf owns model focus,
    `render_pane` drops any standing egui keyboard focus
    (`Response::surrender_focus`). Without it, egui's own memory
    re-installs a past focused widget on remount (workspace re-entry) and
    the next frame's transition reconcile stole model focus back from the
    browser leaf while the OS keyboard sat in the webview child (observed:
    a phantom `focus -> p1` right after each re-select in the pre-fix logs;
    zero after). A real click bypasses through the claim, and
    `set_active_focus` re-arms the terminal's pending focus, so a
    click-claim recovers its widget's focus within one frame.
- **Teardown attribution hardened (harness)**: the smoke's "browser
  processes exit with the app" check switched from a machine-wide
  msedgewebview2 count delta to exact attribution — process pids whose
  command line carries the per-surface udf path (`Win32_Process` query);
  the owned set captured before `WM_CLOSE` must empty afterward. The single
  count-based failure this session was ambient churn (±1 foreign instance
  against a baseline of 11 unrelated msedgewebview2 processes); the
  marker-based check is churn-immune and strictly stronger.
- **Harness robustness (small)**: the smoke's final stderr-tail dump no
  longer dies on a cp1252 console when app stderr carries lossy-decoded
  text (U+FFFD); stdout/stderr are reconfigured with `errors="replace"`.
  The CLI smoke's `identify --json` call gained the py harness's documented
  retry (4 attempts): the FIRST app-plane call after spawn can race the
  shell-startup PTY burst and miss the reply budget once (measured 478 ms
  in that window vs 11-15 ms once settled; M4.1-era behavior, load
  sensitive, unchanged semantics).

### Residuals (updated)

- **Blocking COM calls on the frame thread** — unchanged (hypothesis-level;
  only one historical transient; needs a frame-budget measurement with the
  browser under load).
- **History readback** — unchanged (snapshot-based).
- **Per-surface user data folder kept after close** — decision stands (keep;
  restore speed and profile parity outweigh eager deletion until a port
  shell-launch flow makes profile mutation real).
- **CDP automation subset (feature row 18)** — next milestone slice: the
  snapshot/eval/click/type P0 subset over `CallDevToolsProtocolMethod` and
  the persistent-socket client shape folded in with it.

## Amendment 3 (2026-10-22): M5.2 landed — the CDP P0 automation subset

**Status: landed** (`browser.snapshot`, `browser.eval`, `browser.click`,
`browser.dblclick`, `browser.type` over `CallDevToolsProtocolMethod`, plus
the persistent-socket harness client folded in per ADR-0005 Amendment 5 —
the CLI wrapper for these verbs stays deferred). Verified on the final
binary by the live smoke `port/harness/test_browser_m51.py` (60 checks PASS,
two consecutive runs) plus the unit suite (269 tests, fmt / clippy
`-D warnings` clean) and every earlier baseline re-run on the same binary
(M4.1 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, CLI 8/8, M3.4 turn PASS).

### Design: the deferred-reply plane

- `serve_ipc_calls` never blocks the frame thread on a CDP completion. It
  validates and shapes the automation verb (`prepare_cdp_call`: browser
  leaf, wired host, ref lookup), issues the first CDP call through
  `BrowserHost::call_cdp`, and parks the reply channel in
  `App.pending_cdp` (`PendingCdp{id, surface_id, kind, step, started,
  reply}`). The WebView2 pump thread pushes completions into a host-level
  outbox and wakes the frame loop (`FrameWakeFn`); the frame drains
  `take_cdp_completions()` per host after each `host.tick` in
  `sync_browser_surfaces` and runs the pending's state machine
  (`advance_cdp`). A reply typically leaves on the issuing frame or the
  next, inside the 400 ms app reply budget. Late completions (the server
  already answered its structured timeout, or the pane closed mid-call)
  are logged and dropped; stale pendings are swept at budget × 2 + 100 ms.
- **Step machine** (`CdpStep`, unit-tested parse-level): eval and snapshot
  are a single `Runtime.evaluate` (`CdpStep::Final` shapes the reply).
  click/dblclick issue three CDP calls: (1) a fresh-center evaluate (the
  element may have moved since the snapshot), (2) `Input.dispatchMouseEvent`
  `mousePressed` at that center (`button:"left"`, `buttons:1`, clickCount
  1 or 2), (3) `mouseReleased` — the protocol requires the paired events.
  `browser.type` issues (1) a focus evaluate (`el.focus()`), then (2)
  `Input.insertText` with the whole text.
- **Documented divergence**: upstream `browser.type` streams per-key
  events. The port inserts the string in one `Input.insertText` (the
  text visibly lands; keystroke-level effects — keydown handlers, IME
  composition — are a later slice if a consumer needs them).
- **Refs (`@eN`)**: snapshot-local, minted in DOM order, each carrying the
  CSS selector that re-resolves it plus the element center and a label.
  The snapshot reply installs the pane's ref table; every navigation and
  history verb clears it (upstream: refs are ephemeral, scoped to the
  shown document). An unknown ref is `invalid_params` with the
  "snapshot again" hint; an element that vanished between snapshot and
  use is `invalid_state` with the same hint.
- **Injection safety**: selectors are never interpolated. They ride into
  the expressions as `serde_json::to_string` JSON string literals (JSON
  literals are valid JavaScript), so page-controlled ids/names cannot
  break out of the `querySelector` argument (asserted by a unit test).
- **Reply shapes**: eval = `{"surface_id","value"}` (`result.value` with
  `returnByValue:true`, `userGesture:true`; a page exception surfaces as
  `invalid_state` carrying `exceptionDetails.exception.description`);
  snapshot = `{"surface_id","url","title","text","refs":[…]}`; click/
  dblclick = `{"surface_id","ref","click_count"}`; type =
  `{"surface_id","ref","inserted"}`. The snapshot walker skips non-
  actionable subtrees (script/style/svg), marks display:none/visibility:
  hidden elements out, and does not descend behind a control (its label
  is the text). The walker is original port code (upstream parity is
  semantic: refs structure + ephemerality), not a copied upstream tree.

### Verified facts (this amendment)

- The `CallDevToolsProtocolMethod` wrapper shape was verified against the
  registry sources (webview2-com 0.39.1 `callback.rs`): the completion
  closure is a `CompletedClosure<HRESULT, PCWSTR>`, wrapped by the crate
  as `FnOnce(Result<(), Error>, String)`; `call_cdp` fails synchronously
  (and cleans its tracker) only before the async boundary; a wired host
  is required (`invalid_state` otherwise).
- Live P0 flow on the fixture page (one input, a compose-echo button, a
  dblclick counter): eval of arithmetic values and of page state;
  `browser.snapshot` carrying title/url/text and refs `@e1..@e3` in DOM
  order with the right selectors/tags; a synthetic click firing the
  page's real handler (proves `Input.dispatchMouseEvent` reaches the
  page); type landing text in the field (via focus + `Input.insertText`)
  and the click+type+eval chain composing `composed:hi cmux`; a dblclick
  counter incrementing exactly once per paired event (clickCount 2);
  unknown-ref and oversized-expression `invalid_params`; automation verbs
  on a terminal surface `invalid_state`; navigation killing the ref
  table (a click on the old ref no longer resolves).
- **Measurement probe made permanent** (diagnosability, this ADR's
  blocking-COM question): the crate logs slow COM ticks (> 5 ms,
  rate-limited 1/host/s) and slow CDP completions (> 100 ms, round trip
  measured host-side via `CdpTracker`). First-run evidence: slow-tick
  lines appear only at surface lifecycle moments (5.1/11.9 ms during
  restore, 8.8/14.2/17.2 ms while panes open) and NO slow-CDP-completion
  line ever fired — every automation completion stayed under 100 ms.
  Client-side, the smoke measures five back-to-back `browser.eval` round
  trips over the persistent connection: min 10.5 / median 10.7 / max
  21.0 ms — 20-40× inside the 400 ms reply budget.
- The Amendment 1 "blocking COM on the frame thread" residual narrows to:
  no steady-state exceedance observed; only lifecycle moments reached
  5-17 ms ticks, an order below the reply budget.

### Smoke evolution (harness robustness, 43 → 60 checks)

- The automation section runs on the SECOND instance after the restore
  checks: its fixture navigations never perturb the session file the
  first exit wrote (save only happens at exit; this instance's final
  save is read by nothing).
- Two diagnosis-driven hardenings, both run-evidence-backed: (1)
  `workspace.create` once missed the budget in the startup burst — the
  smoke now uses create-with-verification (a late frame may still have
  done the work; idempotence-safe), consistent with the documented M4.1
  transient. (2) The final ctrl+2 gate check missed twice in a row;
  root-caused to ENVIRONMENTAL foreground theft (a live Firefox window
  claims foreground minutes into a run — SendInput refused 0/4 while it
  held it): the smoke now re-asserts app foreground with the established
  Alt trick before chord sections and records `foreground_is_app`
  beside every gate expectation. Two consecutive green runs followed.

### Residuals (updated)

- **Per-key input** (the `fill`/`press`/`hover` P0 family) — a later M5
  slice; `browser.type`'s whole-string insert is the recorded divergence.
- **Snapshot walker honesty**: label truncation at 120 chars, tab-focusable
  non-element nodes ignored, and `center` coordinates are document-LIVE
  (re-resolved at click time, so the snapshot's stored center is
  informational except for the freshly evaluated one).
- **CLI wrappers for the automation verbs** — deferred (IPC acceptance
  only for this slice; downstream agent users reach the verbs through
  the harness/IPC today).
- **Blocking COM calls on the frame thread** — narrowed: no steady-state
  exceedance; lifecycle ticks 5-17 ms (measured, see above).
- **History readback / udf-after-close** — unchanged.

## Amendment 4 (2026-10-22): M5.3 landed — the observation + form/scroll family

**Status: landed** (`browser.get.title|text|html|value|attr|count|box|styles`,
`browser.is.visible|enabled|checked`, `browser.fill`, `browser.focus`,
`browser.check`, `browser.uncheck`, `browser.select`, `browser.hover`,
`browser.scroll`, `browser.scroll_into_view`, `browser.screenshot`,
`browser.focus_webview`, `browser.is_webview_focused` — 22 new IPC methods,
`APP_METHODS` 20 → 42, capabilities now advertises the union of both
tables sorted). Verified on the final binary by the live smoke
`port/harness/test_browser_m51.py` (87 checks PASS, two consecutive runs)
plus the unit suite (282 tests, fmt / clippy `-D warnings` clean) and every
earlier baseline re-run on the same binary (M4.1 16/16, M4.2 13/13, M4.3
19/19, M4.4 8/8, CLI 8/8, M3.4 turn PASS). Eval round trips measured
min 10.6 / median 10.8 / max 21.3 ms (unchanged M5.2 profile).

### Design: `ref` XOR `selector` target grammar

- **Upstream wire reality (verified)**: upstream's CLI passes element verbs
  a CSS `selector` param (`CLI/cmux.swift` ~19460–20135); its
  `resolveRef` machinery is surface-UUID bookkeeping, unrelated to DOM
  refs. The port keeps the M5.2 ref flavor and adds the upstream selector
  flavor to EVERY element verb (including click/dblclick/type): exactly
  one of `params.ref` (`@eN`, normalized like all ref verbs, still
  budget-capped at 256 bytes) or `params.selector` (free text under the
  64 KiB payload budget). Both at once → `invalid_params`
  ("mutually exclusive"); neither → `invalid_params` ("ref or selector
  is required"). Refs resolve against the live snapshot table (stale →
  `invalid_params` with the "snapshot again" hint); selectors ride into
  the page JS as JSON string literals (injection-safe, unit-tested with
  quote/newline-bearing selectors).
- **Reply echoes are shape-preserving**: every element verb echoes the
  consumed flavor under the exact key the caller sent — `{"ref": "@e2"}`
  or `{"selector": "#answer"}` — so a client can correlate the driven
  element in the shape it speaks. M5.2's ref-flow replies are unchanged
  (`{"surface_id","ref","click_count"}`, `{"surface_id","ref",
  "inserted"}`); the new lean action verbs (fill/focus/check/uncheck/
  select/hover/scroll_into_view) reply `{"surface_id","ref|selector"}`.
- **One-evaluate element verbs** (`CdpKind::ElementJs`): each new element
  verb is ONE `Runtime.evaluate` built from the resolved selector; the
  JS returns plain JSON (never a DOMRect or live object, which
  `returnByValue` cannot serialize — `get.box` builds a plain
  `{x,y,width,height}` object). The `get.*` JS guards each read on
  `document.querySelector` returning null (except `get.count`, a bare
  `querySelectorAll(...).length` where 0 is a valid reply).

### Design: the verb semantics (and their upstream deltas)

- `get.text` = verbatim `el.textContent` (no whitespace folding; the
  snapshot's folded lines remain the snapshot's shape). `get.html` =
  `outerHTML`. `get.value` = `el.value` when the element carries one,
  else null. `get.attr` = `getAttribute(name)`; null when absent.
  `get.box` = the live `getBoundingClientRect` (floats). `get.styles` =
  computed `getPropertyValue` map of ONE requested `property`, or the
  documented default set when absent: `display, visibility, color,
  background-color, font-size, opacity`.
- `get.title` is a SYNC lane read (like `browser.url.get`): the shell's
  tick-polled document title, reply `{"surface_id","title"}` — no CDP
  round trip.
- `is.*` probes reply `{"surface_id","value":bool}`. A missing element
  reads as `false` (upstream contrast-free semantics, never an
  `invalid_state`). `is.visible` = nonzero box AND computed
  `display` ≠ none AND `visibility` ≠ hidden (Playwright-approximate;
  `opacity:0`/covered-by-other-elements are NOT considered — documented
  residual). `is.enabled` = not `disabled` and not `aria-disabled`.
  `is.checked` = `el.checked`.
- `fill` replaces the whole value and dispatches `input`+`change` —
  upstream's whole-value replace semantics, not keystrokes; the
  prototype's own `value` setter runs first (the tracked-property guard)
  and `isContentEditable` hosts get `textContent` instead.
- `check`/`uncheck` guard the target type: checkbox and radio inputs
  qualify; anything else answers `invalid_state` ("not a checkbox or
  radio input"). `select` requires a `<select>` (else `invalid_state`),
  matches the option by value OR visible label, and errors
  `invalid_state` when no option matches (upstream matches value only;
  the label fallback is a documented superset).
- `hover` = the click machine's fresh-center step followed by a single
  `Input.dispatchMouseEvent` `mouseMoved` (`button:"none"`, `buttons:0`)
  — a real mouse move, so page `mouseenter`/`mouseover` handlers fire
  (smoke-verified through a mouseenter-driven fixture).
- `scroll` = one `window.scrollBy(dx, dy)` (absent delta = 0), reply
  `{surface_id,x,y}` = the resulting `scrollX/scrollY`. `scroll_into_view`
  = `scrollIntoView({block:"center", inline:"nearest"})`.
- `screenshot` = one `Page.captureScreenshot` (`format:"png"`,
  `captureBeyondViewport:false` → viewport-only), decoded host-side
  (base64 crate, MIT OR Apache-2.0, added to cmux-shell only) and written
  to `%TEMP%\cmux-screenshots\cmux-<sanitized-surface>-<id>-<epoch-ms>.png`;
  the reply carries `{"surface_id","path"}`. Upstream returns
  path/url/png_base64; the port stages the file and reports the path
  (base64-in-reply stays deferred with the CLI wrappers).
- `focus_webview` / `is_webview_focused` are shell-side (no CDP):
  focus_webview activates the surface's workspace (when needed), sets it
  as the workspace's focused leaf, and the next tick's programmatic
  `MoveFocus` lands the real keyboard focus — the same flow a native
  GotFocus takes. is_webview_focused answers whether the surface is the
  ACTIVE workspace's focused leaf right now (`{"surface_id","focused"}`,
  bool). Both reject terminal surfaces with `invalid_state`; both honor
  the documented "blur never deselects the pane" model (no LostFocus
  tracking).

### Documented divergences (updated)

- **`browser.eval` field name**: upstream's CLI sends `script`
  (CLI/cmux.swift:19444); the port's canonical field is `expression`
  (M5.2). Amendment 4 adds the `script` fallback to the parse
  (never both at once — `invalid_params` "mutually exclusive"), so
  upstream-grammar clients work unmodified. Budgets are identical.
- Reply-shape deltas: getters reply `value` (upstream's CLI prints
  `payload["value"]` — same key); `get.count` replies `count`.
- The type/keystroke divergence of Amendment 3 stands unchanged.

### Residuals (updated)

- **`browser.press`/`keydown`/`keyup` and `browser.wait`** — the P0
  remainder, planned M5.4: a key-name→VK table feeding
  `Input.dispatchKeyEvent` and a polling re-evaluation lane.
- **`browser.find.*` (P1), `browser.set_input_files`, full-page
  screenshot** — deferred as recorded in Amendment 3 and the plan.
- **CLI wrappers** — unchanged (deferred; harness/IPC is the consumer).
- **Blocking COM on the frame thread** — unchanged (no steady-state
  exceedance; the M5.3 verbs are one-evaluate, the screenshot one
  CDP call + a small synchronous file write on the shaping frame).

## Amendment 5 (2026-10-22): M5.4 landed — key synthesis + predicate waits (the P0 automation list is complete)

**Status: landed** (`browser.press|keydown|keyup`, `browser.wait` — 4 new
IPC methods, `APP_METHODS` 42 → 46, capabilities advertising the union
sorted). This closes the agent-browser-port spec's P0 automation list (its
press/keydown/keyup and wait families; the remaining listed families —
pdf, tab/window/frame, dialog, network, storage, cookies, trace — were
never in the port's P0 subset). Verified on the final binary by the live
smoke `port/harness/test_browser_m51.py` (99 checks PASS, two consecutive
runs) plus the unit suite (290 tests, fmt / clippy `-D warnings` clean)
and every earlier baseline re-run on the same binary (M4.1 16/16, M4.2
13/13, M4.3 19/19, M4.4 8/8, CLI 8/8, M3.4 turn PASS).

### Design: the key family (`browser.press|keydown|keyup`)

- **Upstream wire reality (verified, `CLI/cmux.swift` 19668–19671 + the
  wait/key param blocks)**: the press family sends `{"surface_id","key"}`
  — one key token, no selector: keys go to whatever has focus. Upstream's
  `--snapshot-after` flag exists on these verbs and is NOT implemented
  (recorded residual).
- **The port's key vocabulary is a Playwright-style table
  (`key_table()`, ~82 rows)**: named keys (`Enter Tab Space Escape
  Backspace Delete Insert Home End PageUp PageDown ArrowLeft/Up/Right/
  Down F1..F12`, plus the four modifier keys themselves), the 26 letters
  and 10 digits with their US shift partners, and 11 US punctuation
  rows (`; = , - . / \` [ \ ] '` with VKs 0xBA–0xDE). `KeyRow` carries
  the unit-checked Windows VK, the CDP `key` label (the page's `e.key`
  — a named key's name, or the unshifted character), and the
  unshifted/shifted characters.
- **Resolution grammar** (`key_plan_of`): `Modifier+Modifier+Key` chains
  (any order, case-insensitive; `ctrl`→Control, `option`→Alt,
  `cmd|command|meta`→Meta), lowercase spelling aliases
  (`return`→Enter, `esc`→Escape, `spacebar`→Space, `left|right|up|down`
  →the arrows, `del`→Delete, `backtick`→Backquote), single-character
  key spellings (letters/digits directly; punctuation by its printed
  character — `+` and a bare space are special-cased around the
  `+`-split), and uppercase letters / shifted punctuation (`A`, `%`,
  `K`) which imply Shift. Unknown names, unknown modifiers, and empty
  chain parts are structured `invalid_params` listing the accepted
  families.
- **Event mapping** (`key_dispatch_params`): a printing key's down event
  is type `keyDown` carrying the resolved `text` (shift-aware; a
  Ctrl/Alt/Meta engagement suppresses the text — Chromium-honest: their
  chars never land), a non-printing key's down event is `rawKeyDown`
  (Backspace/arrows/etc. still edit/navigate off the raw event —
  smoke-verified: `press Backspace` edits the focused input, `press
  Control+a` selects all through the modifier bitmap), and every up
  half is `keyUp` with no text. Each event carries
  `windowsVirtualKeyCode` and the modifier bits (Alt=1, Control=2,
  Meta=4, Shift=8). `browser.press` = the down event then the prepared
  up event (`CdpStep::AfterKeyDown`); `browser.keydown`/`keyup` are one
  event only. Replies echo `{"surface_id","key"}`.

### Design: the wait family (`browser.wait`)

- **Upstream wire reality (verified, `CLI/cmux.swift` 19455–19515)**: the
  params are `selector`, `text_contains` (`--text`), `url_contains`
  (`--url-contains`/`--url`), `load_state`, `function`, and
  `timeout_ms` (default 5000; `--timeout` accepts seconds). The CLI
  comment explicitly gives its socket "response timeout headroom beyond
  the wait deadline so long waits are reported by the app handler".
- **Port semantics**: every GIVEN predicate must become true (AND; one
  predicate = the plain upstream wait). The strict boolean slots are
  built from JSON-encoded literals (never injected code); the `function`
  predicate embeds the caller's JS verbatim — `browser.wait`'s function
  is automation input at exactly `browser.eval`'s trust level. The
  accepted `load_state` values are `load` (`readyState === 'complete'`)
  and `domcontentloaded` (`readyState !== 'loading'`); anything else is
  `invalid_params` at parse. `timeout_ms` must be a non-negative
  integer, default 5 000, capped at 120 000 ms (the last three are
  parse-level `invalid_params`).
- **The poll loop is completion-driven**: the first evaluate polls all
  slots in one documented order (`querySelector` presence, body
  textContent contains, `location.href` contains, readyState, the
  function's truthy value). A false-but-well-formed poll re-arms the
  SAME evaluate (`issue_cdp_input`) — one round trip per evaluation,
  no busy spin — and refreshes the pending's `last_activity` (the sweep
  anchor moved from `started` to `last_activity` so a healthy
  multi-second wait is never swept). A deadline hit at a poll
  completion answers the structured timeout problem
  (`RequestProblem::wait_timeout`: code `timeout`, the deadline ms in
  the message, action "retry with a longer timeout or a weaker
  predicate"). A malformed poll reply (non-array, non-boolean strict
  slot, slot-count mismatch) fails the wait fast with `invalid_state`
  instead of spinning to the deadline.
- **Per-call reply budget (the upstream-headroom parity)**: the pump's
  park for a `browser.wait` call is
  `deadline + WAIT_REPLY_HEADROOM_MS` (8 000 ms — upstream's CLI uses
  the same `waitMs + 8 s` socket headroom formula) so the app handler
  reports the real outcome instead of the generic 400 ms envelope;
  every other method keeps `APP_REPLY_BUDGET` unchanged. A satisfied
  wait replies `{"surface_id","waited_ms"}` (elapsed since the first
  issue).

### Documented divergences (updated)

- **Key vocabulary is port-side**: upstream's CLI passes the raw token
  to its app (whose interpretation is not visible in the CLI tree); the
  port defines its own documented Playwright-style table above.
- **`browser.wait`'s satisfied reply**: upstream's CLI prints a payload
  it falls back to `OK` for; the port replies `{"surface_id",
  "waited_ms"}`. The deadline error is the port's structured
  `timeout` problem either way.
- **`load_state` subset**: the port accepts `load` and
  `domcontentloaded`; other states (e.g. `networkidle`, which needs
  network instrumentation) are parse rejects, not silent no-ops.
- The eval `expression`/`script` reconciliation of Amendment 4 stands
  unchanged.

### Residuals (updated)

- **The P0 automation families are complete** as of this amendment;
  `browser.find.*` (P1), `browser.set_input_files`, and full-page
  screenshot remain deferred as recorded in Amendments 3/4.
- **`--snapshot-after`** (upstream's CLI flag on the action verbs,
  verified `CLI/cmux.swift` 19554–19556): never implemented; recorded
  here for the first time as a residual.
- **CLI wrappers** — unchanged (deferred; harness/IPC is the consumer).
- **Blocking COM on the frame thread** — unchanged (the M5.4 verbs add
  no new CDP shape: key events are the click machine's two-call
  pattern, waits are one-evaluate re-polls; the wait poll loop parks
  its caller up to deadline+8 s by design, one connection at a time,
  mirroring upstream's serialized per-socket command space).
## Amendment 6 (2026-10-22): M6 warm-up landed - the cmux browser CLI wrappers

Upstream's cmux browser CLI tree (CLI/cmux.swift runBrowserCommand,
~19460-20135) is the consumer surface for the automation verbs; the port
landed its wrapper in port/crates/cmux-cli/src/main.rs over the 37
landed browser.* wire verbs. Verified facts:

- **Grammar mirrors upstream**: rowser [surface] <verb> [args]
  with --surface as the spelled-out alternative; wait options
  --selector --text --url-contains|--url --load-state --function
  --timeout-ms|--timeout; the click family
  (click/dblclick/hover/focus/check/uncheck/scroll-into-view plus the
  scrollinto|scrollintoview aliases), the key family
  (press|key -> press, keydown, keyup), the get fork
  (url|title|text|html|value|attr|count|box|styles), the is fork
  (visible|enabled|checked), eval --script or a positional
  script, select --selector --value, scroll --dx --dy,
  screenshot --out, and the handle-free open family
  (open/open-split/new). Strict unknown-flag rejection (upstream
  behavior); --json accepted anywhere on the line.
- **Wait budget divergence (recorded)**: upstream's CLI socket
  budget is max(15 s, waitMs/1000 + 8 s); the port uses
  max(15 s, waitMs + 9 s) - one second of CLI grace over the
  app's waitMs + 8 s pump park, so a deadline hit always
  surfaces as the app's structured 	imeout problem instead of a
  CLI transport timeout. Every other verb keeps a 500 ms CLI
  budget (the app enforces 400 ms first), so slow verbs surface
  the app's structured retry answer.
- **Element targets**: @eN tokens route to the wire's 
ef key
  and everything else to selector (the wire is
  ref-XOR-selector, verified in cmux-ipc). Upstream's bare-eN
  ref spelling is NOT accepted: a bare token stays a CSS selector
  (conservative port-side choice; snapshot output always prints
  the @ form).
- **Surface grammar mirrors upstream's positional-handle rule,
  including its misroute**: when no --surface flag is given, a
  first token that is neither a flag, nor a handle-free verb
  (open/open-split/new), nor an unsupported verb becomes the
  surface handle - so rowser click #send without a surface
  parses 'click' as the handle and answers the grammar error the
  same way upstream misroutes. Scoped verbs therefore take the
  handle FIRST (rowser <surface> click ...); the CLI's
  'requires a surface handle' guard remains on every scoped arm as
  the documented wire-level contract (the wire itself would
  default to focus; the CLI never does). open navigates the
  focused pane or opens one: the port always places new browser
  panes as a split (placement constant) and the plain reply
  prints OK surface=... pane=... placement=split.
- **Unsupported verbs answer a plain rejection**: the upstream
  verbs outside the port's landed wire answer
  'browser <verb>: not supported by the port (recorded residual;
  see docs/architecture/feature-parity.md)' and exit 1. The scan
  lives in the VERB SLOT ONLY - argument slots are never
  verb-scanned, so selectors and URLs may legitimately spell like
  an upstream verb (get count input and open history work);
  the slot rule is unit-locked by
  irst_token_is_surface_never_eats_verb_words.
- **Refused-unbuilt options**: --id-format, --workspace,
  --window, --focus, --profile are rejected as unbuilt
  counterparts (the port has no multi-window/multi-workspace
  selector or profile store yet); the snapshot flag residuals
  (--snapshot-after, full-page) stay deferred per Amendment 5.

### Bugs caught live by this amendment's smoke section

The extended smoke-cli.ps1 browser section exists precisely to
gate live behavior; its first run caught two CLI bugs, both fixed:

1. **The wait arm never sent the wire 	imeout_ms**: the CLI
   computed its own budget from --timeout-ms but the param was
   omitted, so app-side deadlines silently defaulted to 5000 ms.
   	imeout_ms is now sent explicitly; regressions locked by the
   smoke's predicate-wait and structured-timeout checks.
2. **The unsupported pre-scan verb-scanned argument slots**: a CSS
   selector spelling like an upstream verb (get count input)
   was wrongly rejected. Replaced by the verb-slot rule above;
   unit-locked.

### Verification (2026-10-22)

- Gates: cargo fmt clean; clippy -D warnings clean; cargo test
  299 unit tests green (9 in cmux-cli, +1 regression lock);
  cargo build refreshed 	arget\debug\cmux.exe (cmux-shell.exe
  unchanged - the M5.4 binary).
- smoke-cli.ps1 extended to 40 checks (8 M4.1 CLI checks + 32
  browser checks) PASS twice: open grammar and plain printing,
  navigate + a poll-until-ready wait (a cold WebView2 pane
  answers early calls with the structured 'browser surface is
  still initializing' io_error, action retry - the smoke polls
  instead of racing one shot), url/get-title/eval printouts,
  snapshot --json refs (the walker's selector for an id-bearing
  element is exactly #id, so ref extraction is deterministic),
  click by ref with a page-side click counter, fill + readback,
  focus + press Enter observed in the page's key log, select,
  scroll, is visible/is checked, get count, a --function
  predicate wait, the structured timeout past a 500 ms deadline,
  screenshot --out (file copied) and the plain staged path,
  focus-webview + is-webview-focused, history back (two
  navigations first: history has no real entry before the first
  one - about:blank is the app-seeded start), and the grammar
  negatives (no-handle misroute, unknown option, unsupported
  verb).
- Full battery re-run for the shipped-binary change:
  smoke-m34-turn.ps1 PASS; test_automation_m4.py 16/16 (after a
  baseline robustness fix - identify now polls like its ping; the
  app-plane startup wedge is bounded ~1.4 s, see test-strategy
  section 13), test_automation_m42_password.py 13/13,
  test_terminal_ux_m43.py 19/19, test_pool_m44.py 8/8,
  test_browser_m51.py 99/99.

### Documented divergences (updated)

- **CLI wait budget +1 s grace** over upstream's
  waitMs + 8 s socket-headroom formula (upstream's formula
  matches the app pump park exactly; the port intentionally
  exceeds it so the structured reply always arrives first).
- **Positional-handle misroute preserved** upstream-side; the
  --surface flag is the unambiguous spelled form for scripts.
- **open placement is the constant split**; the plain reply
  documents placement=split.
- **fill with empty text flows to the wire**: upstream's CLI
  permits it; the port's wire 
equired_text rejects an empty
  string with the structured problem.
- All earlier verb-semantics deltas (the click pairing, key
  vocabulary, wait AND semantics, eval script/expression) stand
  unchanged per Amendments 4/5.

### Residuals (updated)

- **CLI wrappers over the LANDED surface are complete** as of this
  amendment; wrappers for future wire verbs land with their wire
  slices (browser P1 import/set_input_files, find.*, full-page
  screenshot, snapshot grouped mode per Amendments 3/4;
  --snapshot-after per Amendment 5).
- **The refused-unbuilt flag family** (--id-format, --workspace,
  --window, --focus, --profile) stays refused until the matching
  app capabilities exist.
- **M6 slice 1** (custom commands, feature-parity row 14) is the
  next CLI scope; SSH/accessibility get evidence spikes first per
  the M6 plan.
