# Testing strategy — cmux Windows port

Goal: evidence-based confidence that the port preserves reference behavior,
with an emphasis on the error-prone OS boundaries (ConPTY lifecycle, resize,
cleanup, IPC auth). Mirrors how upstream itself verifies (see
`repository-map.md` §3): fast static checks, focused unit tests, a
protocol-driven end-to-end harness, and explicit "which checks ran" honesty.

## 1. Layers

| Layer | What | Tech | Where |
| --- | --- | --- | --- |
| L0 static | fmt clean, clippy `-D warnings`, no `unsafe` outside edge crates without review | rustfmt, clippy, `cargo deny` (license/advisory gate) | every PR |
| L1 unit | layout math (port of `CmuxPanes/Geometry` equalize/spatial order), config store (JSONC round-trip), OSC ingest classifier, notification unread models, IPC request parser | `cargo test -p <crate>` | per-crate |
| L2 integration (OS boundary) | real ConPTY sessions: spawn pwsh/cmd, run commands, resize matrix, terminate-while-busy, concurrent sessions, job-object cleanup (no orphaned `conhost.exe`); pipe IPC client↔server with auth modes | tokio + portable-pty in-process runner | `cmux-pty-port`, `cmux-ipc` |
| L3 golden captures | record PTY byte streams from known commands (prompt, `dir`, vim open/quit, cargo test output) and assert emulator state (grid text, cursor, title) — the "answers ConPTY probes" regression guard | replay harness in `cmux-testsupport` feeding `TerminalEmulator` implementations | `cmux-emu` (runs both backends once ADR-0002 fallback exists) |
| L4 protocol E2E (behavior parity check) | replicate the upstream pattern: Python client over the port's IPC (`cmux.py` style: new_workspace, send, read_screen) asserting visible terminal state; run the subset of `cmux-main/tests_v2` assertions that do not require macOS-only features against the port | pytest (Python 3.14 present) | port repo + borrowed assertions |
| L5 manual matrix | pwsh/cmd/WSL shells; high-DPI + multi-monitor; CJK/IME entry; clipboard round-trips; notification toasts; installer smoke | checklist in this file's PR template | release candidates |

The actionable, owner-facing L5 checklist lives in
`docs/testing/owner-manual-matrix.md` (written M4.3 close-out: how to run
the port for a session + per-row expected results; tick boxes per row).

## 2. Windows-specific correctness rules

These encode verified ConPTY behavior on the development machine (ADR-0003
verification section, `rea-findings.md` Q5):

- **Writer lifetime**: the PTY writer must stay alive for the whole session;
  dropping it kills the child (`0xC000013A`). Enforced by construction in
  `cmux-pty-port` + documented in `cmux-shell`.
- **No EOF on child exit**: terminal output is drained by a reader thread
  keyed on child lifetime; EOF-based reads hang and are forbidden in PTY
  test code (the `drain` helper documents this restriction).
- **`portable-pty` pinned to 0.8.x**: 0.9.0's `Child::wait()` hang is a
  tracked upstream regression; upgrading requires re-running the probe and
  this suite, noted in the crate's Cargo.toml comment.
- **Process cleanup is a test target, not a hope**: every test that spawns a
  shell must assert descendants are gone (Job Object states + pid checks).
  REA's Windows PTY-capture limitation (descendant-cleanup verification —
  `rea-findings.md` Q2) is the reference cautionary tale.
- **ConPTY re-encoding**: titles/OSC/noise emitted by ConPTY must be tolerated
  (asserted against golden captures, not exact byte equality except where
  stable).
- **Resize storm test**: rapid WM_SIZE stream must not wedge child or emulator
  (upstream has a coalescing policy — port the concept).
- **No logging of terminal input/secret env in captured fixtures** (manifest
  §11): fixtures redact sensitive frames; regression test asserts scrubbing.

## 3. CI recipe (proposed)

- GitHub Actions `windows-latest`: L0 + L1 + L3 on every PR; L2 (real shells)
  on the same runner; L4 optional as it needs no extra services.
- Weekly cross-check on `ubuntu-latest` for OS-neutral crates.
-Rust toolchain pinned via `rust-toolchain.toml`; Zig 0.16.0 only if the
  from-source engine path is used (cache the built static lib as an artifact).

## 4. Honesty rules (from manifest §15)

- Every report names: what ran, what passed, what was skipped and why.
- A build/parse is not a test run; "tests pass" means the listed L-levels ran
  green on the exact commit.
- Regression commits precede fixes (upstream convention, kept here): commit
  the failing test first, then the fix in the same PR.

## 5. Current verification status (recorded at plan time)

- No Rust toolchain on the machine yet → L0-L3 unrunnable (see
  `building-windows.md`; installation pending approval).
- Reference-side runnable today: `python scripts/verify-local.py` under
  `cmux-main/`, biome via `npx`. Rust crates in `cmux-main/cmux-tui/` cannot
  build here (Zig + ghostty submodule missing).
- REA runtime PTY capture: unsupported on Windows (verified typed error);
  ConPTY evidence comes from L2/L3 once the slice exists.

## 6. Verification status update (2026-10-20, M4.3)

- fmt / clippy (`-D warnings`) / cargo test green on the workspace: **245
  tests** across 13 test binaries (+7 over M4.2: the fonts planner/composer
  unit tests in `cmux-shell` — primary preference, Consolas fallback,
  no-disk defaults, coverage order with gaps, family composition + registry
  keys, unreadable-face skip, all-unreadable no-op). Binary-level shape
  otherwise unchanged from the M4.2 recount (config 14, hooks 37, ipc 32,
  layout 27, pty-port 2+2, shell 80+32, toast 8, workspaces 11,
  cli/testsupport/pty-lib 0).
- Live smokes (all re-verified against the rebuilt workspace):
  - M4.3 terminal-UX battery `port/harness/test_terminal_ux_m43.py` **19/19
    PASS, twice on the final binary** — SendInput/WM-injected real-input
    lane for rows 5/6/7 (typing, arrows, Ctrl+C interrupt, drag selection →
    clipboard, paste, wheel scrollback, CJK burst, resize hammer, WM_CLOSE
    + session write) plus the app-plane plumbing checks.
  - M4.2 password/automation smoke
    `port/harness/test_automation_m42_password.py` 13/13 PASS (unchanged
    shape; details in the 2026-10-20 M4.2 ledger in
    `windows-port-plan.md` §7 and (unchanged) the password-mode red line
    below).
  - M4.1 harness smoke `port/harness/test_automation_m4.py` 16/16 PASS.
  - CLI smoke `port/harness/smoke-cli.ps1` 8/8 PASS (moved here from the
    temp scratch dir at M4.3 close-out).
- M3.4 turn smoke `port/harness/smoke-m34-turn.ps1` 9/9 PASS (3×) after
    the fixture fix below (also moved here from temp).
- M3.4 fixture fix — OS-focus pin: the one regression failure this
  milestone (`desktop=false` → `focused-inline`) was **not** app behavior:
  a plain launch occasionally puts the new window in the OS foreground
  before the delivery instant, and the arrival policy correctly answers
  focused-inline (upstream parity: a focused surface gets inline feedback
  only). The smoke now waits for the app window, then pins the foreground
  to the shell desktop once (ALT-trick + `SetForegroundWindow`), so the
  delivery-time focus state is owned by the fixture. Evidence: the failing
  run's own log line (`focused-inline … t=1.831s`), 3/3 PASS after the pin.
  - M3.4 turn smoke `port/harness/smoke-m34-turn.ps1` 9/9 PASS (3×) after
    the fixture fix below (also moved here from temp).
- Live-input harness technique (M4.3-verified; rules for future lanes):
  - egui collates raw input per frame, and a debug build frames at
    ~30-200 ms. A synthesized click must press and release in *separate
    frames* (down → ~80 ms → up; coalesced down+up is swallowed and the
    click never registers), and a synthesized drag needs dwell pacing
    (0.5 s around the button events, 0.35 s per intermediate move) so the
    engine's gesture code sees cell-changing frames.
  - Coordinate spaces differ by DPI: the app's chrome numbers (sidebar
    width 216) are egui *logical* points, while SendInput pushes physical
    screen pixels. Scale by `GetDpiForWindow/96` or the press lands on the
    sidebar (M4.3 evidence: the unscaled press kept `contains_pointer`
    false). This one artifact is also what mis-described multi-row drags
    as impossible; post-fix, a multi-row drag selects and copies.
  - Keyboard layout: pin a US layout per window before chord assertions
    (HKCU's `Preload` can carry a layout — Arabic here — that remaps `C` to
    `ؤ` under focus, breaking every Ctrl-chord); `LoadKeyboardLayoutW` +
    `WM_INPUTLANGCHANGEREQUEST` at startup.
  - Clipboard: read/write via user32 with explicit `c_void_p` argtypes and
    `c_wchar_p` casts; poll after copy actions (arboard's write is lazy
    under injection). A failing copy assert that prints the *previous*
    run's paste marker is the ghost-marker signature — the poll window
    closed too early, not an app bug.
- Password-mode red line kept green by unit tests without touching
  process env or real DPAPI in the server-enforcement layer: servers spin
  on injected password-provider closures; the real env/file/DPAPI paths
  run in the store's own unit tests and the live smoke (env source only;
  the default DPAPI file is never written by smokes).
- Harness-lane caveat for CI: the assert-script style needs no pytest, but a
  dedicated lane entry (Python 3.x on the runner + a debug build) must be
  added when CI lands (§3); the M4.2 smoke launches two app instances and
  the CLI binary, so the lane needs both binaries built. The M4.3 battery
  additionally needs a real interactive desktop (SendInput + clipboard +
  window management) — a headless CI runner cannot serve as its lane.

## 7. Verification status update (2026-10-20, M4.4)

- fmt / clippy (`-D warnings`) / cargo test green on the workspace: **247
  tests** (+2 over M4.3, both in `cmux-ipc`:
  `a_parked_request_does_not_hold_the_pipe` — a first connection stalled
  inside the handler 900 ms while a second client's exchange completed
  well inside it — and `every_pooled_connection_receives_its_own_reply` —
  4 concurrent in-flight requests, each envelope carrying its issuer's
  id). Binary-level shape otherwise unchanged: config 14, hooks 37,
  **ipc 34**, layout 27, pty-port 2+2, shell 80+32, toast 8, workspaces
  11, cli/testsupport/pty-lib 0.
- New live smoke `port/harness/test_pool_m44.py` **8/8 PASS, twice on the
  final binary**: readiness (app-plane `identify` polled — `ping`/
  `capabilities` are server-inline and do *not* prove the frame loop), the
  M4.1 identify shape as a regression, 4 concurrent harness clients × 8
  rounds (32 exchanges, every reply in its own envelope — `_call`'s
  id-match raise is the cross-talk detector), a **silent-peer starvation
  guard** (a ctypes peer holds one connected slot, never writes; the CLI
  answers in 0.01–0.05 s — the pre-M4.4 single-instance server would have
  stalled the open until the peer disconnected), a same-name second
  instance claiming and logging `IPC disabled` while the first keeps
  serving, and a no-panic sweep of the app stderr.
- Technique note for the pool lane: client connections are per call
  (`cmux.py::_call`), so concurrency is expressed with threads of
  long-lived `cmux(...)` instances; the silent peer uses a raw
  `CreateFileW` hold in a throwaway fixture script (inline `-c` quoting is
  fragile on this toolchain). The smoke reuses the M4.1 launch/teardown
  conventions (unique `CMUX_SOCKET_PATH`, isolated session/config envs,
  `taskkill /T` fallback) and runs fully in `cmd`-invoked python with no
  SendInput lanes, so it is CI-lane-compatible once the debug build +
  Python lane exists (§3).
- Regression baselines re-run on the final binary: M4.1 16/16, M4.2 13/13,
  CLI 8/8, M3.4 9/9, M4.3 19/19.
- Owner-facing L5 checklist landed at this milestone:
  `docs/testing/owner-manual-matrix.md` (manual verification the machine
  lanes cannot own: real IME composition, toast/latency feel, multi-chord
  ergonomics; referenced from the L5 layer row above).

## 8. Verification status update (2026-10-21, M5.1)

- fmt / clippy (`-D warnings`) / cargo test green on the workspace: **255
  tests** (+8 over M4.4: `cmux-ipc` +2 —
  `parses_browser_verbs_with_optional_ids_and_urls` (wire names, optional
  ids/URLs) and `browser_url_violations_carry_invalid_params` (empty/absent
  URL → `invalid_params`); `cmux-webview2` +3 — two DIP-rect device-pixel
  rounding tests plus `an_accel_router_enqueues_only_matching_keys_and_drains_in_order`;
  `cmux-shell` bin +3 — digits map to workspace actions only at
  `action_for_accel_key` (0x31→row 0, 0x39→row 8; ctrl+0 and F1 stay
  unset), a browser pane's user-data folder nests per surface id,
  `new_browser_pane` defers the host and has no display title yet).
  Binary-level shape: config 14, hooks 37, ipc 36, layout 27, pty-port
  2+2, shell 80+35, toast 8, webview2 3, workspaces 11,
  cli/testsupport/pty-lib 0.
- New live smoke `port/harness/test_browser_m51.py` **39/39 PASS on the
  final binary** (plus a full restart section using the same session file):
  IPC readiness/identify, `browser.open_split` (surface + model focus),
  page readiness by **title** (not phase — see the lesson below), the
  navigate/back/forward/reload fan over file:// pages, seven structured
  error gates (`invalid_state` browser-vs-terminal both ways, `not_found`,
  `invalid_params` ×2, surface-routing injections), msedgewebview2 process
  tree per surface, main-window enumeration, child-HWND rect pin (±16 px),
  per-surface user-data folder, ctrl+2 in BOTH routes (egui: keys on the
  main window; router: native focus proven inside the webview child via a
  per-thread `GUITHREADINFO` scan, F1 probe press logging the accelerator
  event), hidden-pane survival across a switch, WM_CLOSE teardown + UDF
  kept, session snapshot carrying `browserUrl`, restart restore (pane
  kinds, saved page reload, identify replaying the saved focused id from
  session.json), panic sweep.
- Harness lessons captured for future webview lanes:
  - **Poll page state by document TITLE, never by host phase**: the host's
    phase flips to ready at wire, before the async `Navigate` commits — a
    phase-only poll stops on `about:blank` (caught on the restore leg;
    URLs on their own race the same way once navigation is in flight).
  - **`SetForegroundWindow` fails silently under the OS foreground lock**;
    the M4.3 ALT-trick (`foreground_window`) is mandatory before chord
    presses (one run's "no switch" was the synthesized keys going to
    whatever held the foreground, not the app).
  - **One reply-budget timeout is transient, not fatal**: the frame can
    exceed 400 ms once under browser-process load; smokes retry app-plane
    calls (4 attempts) and verify STATE (workspace.current) rather than a
    lost reply.
  - Evidence is never lost with the fixture: the runner now dumps both
    app-stderr tails on every outcome; the accelerator/`focus ->`/`ipc
    serve` diagnostic lines (ADR-0006 Am. 1) are the lane's permanent
    evidence channel.
- Regression fix folded into this milestone with its evidence: the frame's
  model-focus reconcile (egui focus transitions only) — root-caused from
  run logs (`focus -> p3` then identify reading p1), fixed, and locked by
  the "browser.open_split focuses the new leaf (model focus)" +
  "identify carries the browser facts" checks which failed on the pre-fix
  binary and pass on it (3 runs).
- Regression baselines re-run on the final binary: M4.1 16/16, M4.2 13/13,
  M4.3 19/19, M3.4 9/9, CLI 8/8, pool 8/8 twice.

## 9. Verification status update (2026-10-21, M5.2 residual slice)

- fmt / clippy (`-D warnings`) / cargo test green on the workspace: **260
  tests** (+5 over M5.1 close: `cmux-webview2` +2 —
  `a_chord_routes_only_while_ctrl_is_actually_held` (registered key × Ctrl
  state combos through `routes_as_chord`) and
  `native_focus_gains_drain_per_surface_in_order` (the GotFocus queue);
  `cmux-shell` bin +3 — the reconcile's corner states on the pure
  `focus_reconcile_target`: click claims without an egui transition, a
  click on the already-focused pane is a no-op, and a no-click transition
  routes while standing focus unchanged does not). Binary-level shape:
  config 14, hooks 37, ipc 36, layout 27, pty-port 2+2, shell 80+38, toast
  8, webview2 5, workspaces 11, cli/testsupport/pty-lib 0.
- Live smoke `port/harness/test_browser_m51.py` **43/43 PASS on the final
  binary** (the 39 M5.1 checks unchanged, plus): plain digit through the
  webview does not switch, alt+digit does not switch, ctrl+2 still routes
  after the gate (all three while native focus is proven inside the webview
  child), the app-owned webview process set carries the per-surface udf
  path in its command lines and is empty after `WM_CLOSE`, one attribution
  probe right after the surface's process peak.
- Harness lessons added for future lanes:
  - **Machine-wide process COUNT checks lose to ambient churn**: the
    "browser processes exit with the app" check failed once with 12 active
    msedgewebview2 processes against a baseline of 11 (the baseline itself
    was 11 FOREIGN instances at smoke start — this machine runs many). The
    check is now exact pid identity: `Win32_Process` command lines
    containing the per-surface udf path define the app-owned set; that set
    must empty after `WM_CLOSE`. Churn-immune and strictly stronger.
  - **A cp1252 console plus lossy-decoded OS text (U+FFFD) in app stderr
    once killed the final tail dump** (`UnicodeEncodeError` after all
    checks had passed); the smoke now reconfigures stdout/stderr with
    `errors="replace"` at startup and the dump survives any codec.
  - **Diff run-to-run diagnostic sequences when chasing focus bugs**: the
    phantom `focus -> p1` reclaim (egui re-installing a remounted widget's
    past focus one frame after a workspace re-select, while the OS keyboard
    sat in the webview child) was invisible in outcome-level checks — only
    the permanent `focus ->` log channel exposed it; the post-fix run has
    zero such lines (counted against the pre-fix capture).
  - **One-shot CLI calls race the shell-startup PTY burst**: `smoke-cli.ps1`'s
    `identify --json` failed twice then passed on the same binary; a
    dedicated probe measured 478 ms for the first app-plane call after
    spawn vs 11-15 ms once settled — inside the burst window the budget
    (400 ms) can lapse once. The runner adopted the py harness's documented
    retry (4 attempts); the app code path is M4.1-era and unchanged, so
    this is a harness flake class, not a port regression.
  - **PowerShell's `>` redirection of merged child output writes UTF-16LE**
    (each ASCII char followed by a NUL): grep/decode evidence captures with
    a utf-16 codec first or the text reads as garbage.
  - **SendInput-timing flakes exist in terminal-UX lanes too**: M4.3's
    ctrl+c check failed once and retried green on the identical binary —
    re-run once before diagnosing a code regression.
- Regression baselines re-run on the M5.2 final binary: M4.1 16/16, M4.2
  13/13, M4.3 19/19 (after the flake retry above), M4.4 8/8, CLI 8/8
  (hardened runner), M3.4 turn runner PASS; browser smoke 43/43 on the same
  binary with the focus-unification sequence verified from the app-stderr
  tail (`accelerator captured: focus -> p3` on restores, zero
  `focus ignored` lines).

## 10. Verification status update (2026-10-22, M5.2 CDP automation subset)

- fmt / clippy (`-D warnings`) / cargo test green on the workspace: **269
  tests** (+9 over §9 close: `cmux-ipc` +4 — the five application-verb
  parses (happy paths, `@eN` ref normalization across `@`/`e`/case
  spellings, budgeted free-text, method/routing advertisement);
  `cmux-shell` bin +4 — the automation plane's pure pieces
  (`cdp_kind_of` request mapping incl. clickCount 1/2 and the sync verbs
  staying off the plane, `decode_cdp_outcome` failure classes
  (COM error / non-JSON / `exceptionDetails`) with
  `evaluate_value_of`, the fresh-center parser's gone/shapeless
  rejection, and JSON-literal selector embedding in the
  querySelector/focus builders); `cmux-webview2` +1 —
  `cdp_probe_tracks_one_wall_clock_per_call` (one `CdpTracker` mark per
  issue, taken once on completion). Binary-level shape: config 14, hooks
  37, ipc 40, layout 27, pty-port 2+2, shell 80+42, toast 8, webview2 6,
  workspaces 11, cli/testsupport/pty-lib 0.
- Live smoke `port/harness/test_browser_m51.py` **60/60 PASS on the
  final binary, twice consecutively** (43 M5.1/keyboard checks + 17
  automation checks). The automation section runs on the SECOND app
  instance after the restore checks, so the fixture's navigation never
  perturbs the session file the first exit wrote (save-on-exit only;
  that instance's final save is read by nothing). New checks cover:
  eval round-trip latency (client-side measurement printed as an INFO
  line even on pass — min 10.5 / median 10.7 / max 21.0 ms against the
  400 ms reply budget), snapshot identity/refs (`@e1..@e3` in DOM order,
  selector/tag/label fields, heading in text), unknown-ref and
  oversized-expression `invalid_params`, synthetic click/dblclick/type
  driving the fixture page's real handlers with the click+type+eval
  chain composing `composed:hi cmux` (a dblclick increments its counter
  exactly once per paired event), automation-on-terminal
  `invalid_state`, and navigation killing the ref table.
- App-side measurement evidence (grep of both stderr logs): slow-tick
  lines only at surface lifecycle moments (5.1/11.9/12.4 ms around the
  restore, 8.8/14.2/17.2 ms while panes open — an order below the reply
  budget) and **zero** slow-CDP-completion lines (threshold 100 ms with
  `CdpTracker` host-side round trips): every automation completion
  stayed under that.
- Harness lessons added for future lanes:
  - **`cargo test` does not refresh `target/debug/cmux-shell.exe`** — one
    smoke ran against the PREVIOUS segment's binary and answered
    `unknown_method` for a verb that existed in the workspace tests;
    a timestamp check (exe vs sources) exposed it. Any live smoke must
    follow a real `cargo build` (the runner now implicitly does because
    it asserts the exe path, but the lesson is: verify the artifact's
    timestamp, never assume test-driven freshness).
  - **Background desktop apps steal foreground mid-run and SendInput
    refuses wholesale (0/4 events) while the thief holds it** — this is
    the root-caused variant of the M4.3/M5.2 SendInput flake: a live
    Firefox window (a normal user app on the smoke host) claimed
    foreground minutes into the run, so a chord stream aimed at the app
    produced no route. The chord sections now re-assert app foreground
    with the established Alt trick before pressing and record
    `foreground_is_app` beside every expectation; two consecutive green
    runs followed. Chord checks that pass by ABSENCE (the plain/alt
    gate negatives) silently absorb this class — only a routing
    positive is achingly sensitive; keep the foreground evidence in the
    failure detail.
  - **Non-idempotent creates need verification, not blind retry**:
    `workspace.create` once missed the reply budget inside the
    documented startup burst, but the frame may still have done the
    work after the server thread gave up — the runner now treats
    "workspace-row count grew" as success and only otherwise retries
    (a blind retry would double-create).
  - **The persistent harness client (ADR-0005 Amendment 5) changes what
    a smoke counts**: one pipe per client lifetime, handshake once per
    connection, transport-level failures reset it, a parsed non-ok
    reply does NOT — write assertions against CALLS and behaviors, never
    against connection counts.
  - **Oversize fixtures must sit between the two admission limits**: the
    plane rejects free text at 64 KiB (payload budget, `invalid_params`)
    while the server's frame read admits up to 1 MiB — a "too big"
    fixture that is actually under the payload budget executes normally
    (one 9-kilobyte expression did); 70 KiB rejects at the plane
    deterministically.
- Regression baselines re-run unchanged on the final binary: M4.1 16/16,
  M4.2 13/13, M4.3 19/19, M4.4 8/8, CLI 8/8, M3.4 turn PASS; the
  browser smoke's first-30 checks (M5.1 set) all stayed green across
  five runs of the day.

## 11. Verification status update (2026-10-22, M5.3 observation + form/scroll family)

- Gates on the final binary (built after all M5.3 code edits): fmt /
  clippy `-D warnings` clean; unit suite **282** (ipc 40 → 46 with the
  target-dual-flavor parse tests, the eval `script`-fallback tests, and
  the reply-echo helpers; shell bin 42 → 49 with the M5.3 JS builders,
  action-sentinel parse, state-probe parse, scroll parse, screenshot
  stream helpers, and the M5.3 kind-mapping tests).
- Live battery: `test_browser_m51.py` **87 checks PASS, two consecutive
  runs** (the smoke's PAGE_FORM fixture grew the checkbox/select/hover/
  hidden/tall-scroll elements; the ref-table assertion moved from
  `@e1..@e3` to `@e1..@e6` with the same per-ref selector/tag checks; and
  the automation section now drives every M5.3 family end to end —
  getters, probes, form verbs, hover (mouseenter observed through a
  fixture handler), scroll, scroll_into_view, screenshot file lane, sync
  focus verbs, plus the ref/selector negatives: mutual exclusion,
  missing-target, not-a-checkbox, unmatched option, and the terminal-
  surface rejections on the sync verbs). All six earlier baselines re-run
  on the same binary: M4.1 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8,
  CLI 8/8, M3.4 turn PASS. Eval round trips min 10.6 / median 10.8 /
  max 21.3 ms.
- Harness lessons added for future lanes:
  - **Reply-contract checks must not assume page-geometry zero**: the
    `browser.scroll` check first asserted `x == 0` and failed with the
    page's constant 8 px horizontal sliver (a Chromium viewport-rounding
    artifact of the pane; `window.scrollBy(0, +/-500)` moved y exactly
    and never touched x). The honest contract assertion is the delta the
    verb owns (y = 500 → 0) plus stability in what it does not own;
    asserting `x` unchanged expresses the same guarantee without coupling
    to the fixture's ICB rounding.
  - **A delayed chord positive needs its own foreground re-assert**: the
    M5.2 gate checks re-assert app foreground at section start, but the
    final ctrl+2 routing positive fires minutes later — one stolen
    foreground (the recorded Firefox pattern) made it fail with
    `foreground_is_app=False`. The press now re-asserts foreground
    immediately before that chord; the run then went 87/87 twice.
  - **File-lane assertions are cheap and load-bearing**: the screenshot
    reply's `path` is verified by reading the file's first four bytes
    (`\x89PNG`). A path with a broken stage surfaces as a structured
    `invalid_state`/`io` error instead, so a fake-looking success cannot
    pass silently.
- The smoke's eval-latency INFO line stays the accepted latency probe;
  the M5.3 verbs' round trips are not separately metered (same lane,
  same one-evaluate shape).

## 12. Verification status update (2026-10-22, M5.4 key synthesis + waits)

- Gates on the final binary (rebuilt after all M5.4 code edits; the exe
  timestamp was checked): fmt / clippy `-D warnings` clean; unit suite
  **290** (ipc 46 → 48 with the key-family and wait-family parse tests
  (predicates, the 5000 ms default, the 120 000 ms cap, the integer and
  unknown-`load_state` rejects, and the `wait_timeout` problem shape);
  shell bin 49 → 55 with the desktop-honest VK-table lock, the
  vocabulary resolver (aliases, single-character spellings, modifier
  chains, the `+`/space disambiguation), the keyDown/rawKeyDown/keyUp
  event mapping, the wait builder's slot order + JSON-literal safety,
  the slot parser (AND, JS truthiness, fail-fast malformed replies),
  and the M5.4 kind-mapping tests).
- Live battery: `test_browser_m51.py` **99 checks PASS, two consecutive
  runs** (the PAGE_FORM fixture grew the non-interactive `#keyLog` div
  and its document-level keydown/keyup loggers — refs stayed `@e1..@e6`
  because the walker mints only interactive elements; the automation
  section now drives the M5.4 family end to end: type → `press
  Backspace` edits the focused input, `press Control+a` selects all
  with the modifier bit visible in the page's own log, a second
  `Backspace` clears the selection, `press Enter` shows the full
  down+up pair, `keydown("Shift")`/`keyup("Shift")` land in order
  across two calls; the wait family resolves a selector, url_contains,
  the domcontentloaded+url AND pair, the selector+text AND pair, and a
  self-counting function predicate on a real re-poll loop, while an
  unsatisfied `text_contains` wait answers the structured `timeout`
  past its 500 ms deadline; unknown vocabulary is a structured
  `invalid_params`, and `press` on a terminal surface is
  `invalid_state`). All six earlier baselines re-run on the same
  binary: M4.1 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, CLI 8/8, M3.4
  turn PASS.
- Harness lessons added for future lanes:
  - **A positive assertion for an action the code never issued fails
    in exactly this way**: the first live run's failure was the smoke
    asserting a `D:Enter` log entry while the smoke's own code had
    never called `browser_press("Enter")` — the plan-written check and
    the script body had drifted. The quick locate was a minimal
    single-instance probe (own fixture, own pipe, verbatim `_call`
    replies): three presses through the same wrapper path showed
    `D:Enter;U:Enter` arriving fine, which pinned the defect to the
    smoke body, not the app. Pattern: when a live check contradicts an
    isolated probe, diff MY script against MY assertion first.
  - **Lingering app survivors poison the next battery**: the failing
    first live run left two window-less `cmux-shell.exe` processes
    behind, and the M4.1 baseline then timed out twice at its first
    app-plane call (`system.identify`) while server-plane
    `system.ping` still answered — the signature of a wedged frame
    plane, not a remote regression. Killing the survivors restored
    M4.1 to 16/16 without a single code change. Battery hygiene: check
    for stray `cmux-shell.exe` processes before a baseline series;
    a server-plane-passes/app-plane-times-out split is process-local
    damage, and the logs of the wedge are in the survivor's own
    stderr, not the new run's.
- The wait family's instant predicates resolve inside the 2 s ceiling
  the smoke asserts (one evaluate round trip per poll; no per-poll
  latency metering line — the M5.2 eval INFO line remains the lane's
  latency representative).
## 13. Verification status update (2026-10-22, M6 CLI browser wrappers)

Unit: the CLI crate grew the `cmux browser` grammar (dispatch arm,
usage lines, browser_call + the BrowserPrint printers, the option
parser, the element-target router, the method map, the wait budget,
the unsupported-verb table) with 9 tests (8 grammar/printer tests
then +1 regression lock) - port total 299 green. Gates: cargo fmt
clean, clippy -D warnings clean, cargo build refreshed
target\debug\cmux.exe (cmux-shell.exe untouched - the M5.4 binary
stays what every other baseline runs).

- The browser section of harness/smoke-cli.ps1 (32 new checks; 40
  total; PASS twice) drives the CLI wrappers against a file://
  fixture page: open/open-split grammar and plain printing,
  navigate + a poll-until-ready wait (a cold WebView2 pane answers
  early calls with the structured "browser surface is still
  initializing" io_error, action retry - single-shot races fail by
  design, so the smoke polls), url/get-title/eval value printouts,
  snapshot --json refs (the walker's selector for an id-bearing
  element is exactly #id, so the smoke reads the button ref
  deterministically), click by ref with a page-side click counter,
  fill + value readback, focus + press Enter observed in the
  page's key log, select + readback, scroll, is visible/is
  checked, get count, a --function predicate wait, the structured
  timeout past a 500 ms deadline (exit 1, "timeout" on stderr),
  screenshot --out (the file is copied) and the plain staged path,
  focus-webview + is-webview-focused, history back (two
  navigations first: history has no real entry before the first
  one - about:blank is the app-seeded start), and the grammar
  negatives (no-handle misroute message, unknown option,
  unsupported-verb plain rejection).
- Two CLI bugs were caught by this section on its first live run
  and fixed; both carry regression checks: the wait arm computed
  the CLI budget from --timeout-ms but never sent the wire
  timeout_ms (deadlines silently defaulted to 5000 ms - now sent
  explicitly, locked by the predicate-wait and structured-timeout
  smoke checks), and the unsupported-verb pre-scan ran over every
  token so a CSS selector spelling like an upstream verb
  ("get count input") was wrongly rejected (now verb-slot only;
  unit-locked by first_token_is_surface_never_eats_verb_words).
- Baseline robustness fix (harness file, no app change):
  test_automation_m4.py's first app-plane call (system.identify)
  fired immediately after the server-plane ping/capabilities
  checks, and its one shot kept landing inside the documented
  shell-startup burst - a controlled diagnostic measured the wedge
  bounded at ~1.4 s (attempt 1 fails fast at the pump's reply
  budget, attempt 2 answers in ~10 ms once the app settles).
  identify now polls like the file's ping (30 s deadline) and the
  baseline reads 16/16. The diagnostic script was deleted after
  the measurement per segment scratch hygiene.
- Full battery re-run on the shipped-binary change (only cmux.exe
  rebuilt): smoke-cli.ps1 40/40 x2, smoke-m34-turn.ps1 PASS,
  test_automation_m4.py 16/16, test_automation_m42_password.py
  13/13, test_terminal_ux_m43.py 19/19, test_pool_m44.py 8/8,
  test_browser_m51.py 99/99.

## 14. Verification status update (2026-10-22, M6 slice 1: custom commands)

- The custom-command slice landed end to end with four new unit
  suites: cmux-config (+3: `commands` entries round-trip, decode-time
  validation mirrors upstream's message text, the invisible-charset
  sanitize), cmux-ipc (+1: both verbs' parsing including the bool-only
  `ack` and name rejection), cmux-shell (+3: run-shape checks, list-row
  shaping with sanitized text, broken-entry config load), cmux-cli
  (+1: the per-verb strict flag scanner). Gates: fmt clean, clippy
  `-D warnings` clean, 307 unit tests (299 pre-slice). Both binaries
  rebuilt (the shell changed this slice with the two app-plane arms).
- Wire contract locked by the shell-side check order:
  `command.run` validates the definition BEFORE resolving the
  surface, so an unknown name or an unconfirmed command can never
  mint a terminal write; terminal execution reuses the send_text
  surface rules (browser surfaces decline) and types the sanitized
  text plus Enter through the PTY.
- smoke-cli.ps1 grew 40 to 49 checks: the fixture config (JSONC
  comment tolerated on read) carries a plain echo, a confirm:true
  echo, and a workspace-flavor entry; the new checks cover rows in
  config order, the --json envelope, run typing the command plus
  Enter (marker observed through read-screen), confirm refusal
  without --yes and execution with --yes, unknown name (structured
  not_found), workspace residual, and strict unknown-option
  rejection (one-line error, exit 1 = the browser refusal's code;
  usage() dumps 2). Two full smoke passes.
- Baseline robustness fix (harness file, no app change): the M5.1
  harness's webview focus steps asserted the FIRST click's outcome.
  Across the same-binary battery runs this produced three flake
  profiles (a plain digit switching workspaces mid-gate followed by
  a select raising fatal not_found and aborting the run; a fully
  green run; an empty GUITHREADINFO candidate list so the chord
  never reached the app). A controlled latency probe measured every
  browser verb at 10-45 ms round trip once the app settled (probe
  deleted per scratch hygiene), placing the flake in the
  click-to-focus dance, not the app plane. Both focus steps now
  re-click (up to 5 times, 0.4 s apart) until the GUITHREADINFO
  probe sees the focus owner inside the webview child before the
  checks execute; the checks themselves are unchanged. Recorded
  note: the re-select after the gate uses retry_ipc, whose
  not_found path raises fatally - converting that select into a
  counted check would keep the whole battery reportable in one run
  (deferred; not this slice's scope).
- Full battery re-run on the shipped-binary change: smoke-cli.ps1
  49/49 x2, smoke-m34-turn.ps1 PASS, test_automation_m4.py 16/16,
  test_automation_m42_password.py 13/13, test_terminal_ux_m43.py
  19/19, test_pool_m44.py 8/8, test_browser_m51.py 99/99 (one clean
  run before the settle fix, then 99/99 x2 after).

## 2026-10-24 — lane 3 (sub-header chips + command bar + palette)

- New battery harness `port/harness/test_subheader_lane3.py` — two
  modes against a real launched app and a seeded git fixture
  (`git init` + seed commit + staged edit so the chips have data; the
  workspace root is intentionally not a repo, which also exercises
  the no-repo hidden-chip rule on every other battery lane):
  - visual (17/17): sub-header fill `#191b22`, git chip fill
    `#0c0e14` run, info ink (prompt glyph / `+adds`), amber badge ink
    `[port-main*]`, danger `-dels` ink, agent chip absence while
    idle, keycap pill fills, tray `#11131a` button fills, command
    bar rule (fill, glyph ink, placeholder ink, `Ctrl`/`K` pill
    fills) — all at the monitor DPI (125%).
  - behavior (20/20, twice for stability): bar click and `Ctrl+K`
    open the modal, Esc closes (overlay pixels return to the pane
    interior), Down/Up move the wrap selection, Enter runs the
    selected config command into the focused terminal (marker read
    back through `cmux read-screen`), no stray typing leaks while the
    modal consumes input, tray v-split button splits the pane
    (gutter pixel scan + surface-list count through the CLI).
- Probe-side findings that changed the harness, not the app:
  - `#11131a` (frame fill) vs `#0c0e14` (pane interior) differ by
    only 6/channel — fill assertions at tol 6 are tolerance-dupes
    that PASS with the palette absent. Fixed at tol 4; the failing
    fragments then localized immediately.
  - Glyph ink pixels split a tray button's fill run: the v-split
    button's `#11131a` run is shorter than the scan threshold, so
    the "widest/longest run" heuristic silently clicked the
    EQUALIZE button (a no-op on a single pane — surfaced only after
    cross-checking the pane tree through the CLI). Click targeting
    now takes the FIRST run of at least 4px.
  - Injected input needs an explicit `SetForegroundWindow` before
    each interaction: the app's foreground-activation race swallowed
    bar clicks / key presses non-deterministically (the same
    click-to-focus class as the M5.1 flake ledger entry). With
    foreground pinned, the behavior pass is 20/20 on repeated runs.
- m43's drag anchor and m51's pane-top expectation updated for the
  +32pt sub-header (chrome is 64pt now): test_terminal_ux_m43.py
  19/19, test_browser_m51.py 99/99.
- smoke-cli.ps1 native-stderr hardening (harness file, no app
  change): under the script's `Stop` preference, the documented
  identify/browser-init retry sites print a `NativeCommandError`
  into the redirected stream and abort mid-run (observed twice:
  one-line log after ping; 34-pass m51-style abort was a separate
  py raise). The two retry loops now scope
  `$ErrorActionPreference='Continue'` so the diagnostic prints and
  the loop retries; the check results still decide — 49/49 x2 after.
- Full battery on the lane-3 binary: smoke-cli 49/49 x2, m34 turn
  PASS, M4 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, m51 99/99,
  SSH 28/28, restore 51/51, a11y 3 pass / 4 standing accesskit
  skips / 0 fail, lane-3 probe 17/17 + 20/20, workspace
  `cargo test` 324, fmt + clippy -D warnings clean.

## 2026-10-24 - lane 4 (aside runtime-services segment)

- Live battery `port/harness/test_sidebar_services_lane4.py` **24/24
  PASS three consecutive runs** on the lane-4 binary: launch-empty
  (header ink, no fill/warm/badge), seed via config `command.run`
  (with probe-side netstat pid as cross-evidence), UP row (fill run,
  warm `:port` ink, blue UP badge, fill-pixel volume), owner killed
  via netstat-discovered pid (STOP latch: fill persists, warm/badge
  gone), pane exit (`exit` + CR) closing the last workspace's window
  cleanly (the sampler survives the vanished root; map-level prune is
  unit-tested). Passive PrintWindow captures only; no clicks or keys
  against the window.
- The first run read 10/3 and both failures were real defects, not
  probe noise: (a) the probe's segment table assumed the sidebar rect
  starts at client y=0, but it starts below the 32pt strip - a live
  per-line ink profile over the sidebar column placed the bands
  (+32pt), and the corrected table went into the probe as the verified
  geometry (lesson: geometry tables come from an ink profile, not from
  point arithmetic); (b) the seed exposed the M6 LF-as-Enter defect
  (see the windows-port-plan 2026-10-24 entry): raw `\n` from
  `command.run` put pwsh on the `>>` continuation prompt, the smoke's
  marker check had been passing on the visible-but-unexecuted text,
  and the port now types CR (`command_input_bytes` +
  `executed_commands_end_in_cr_not_lf`). After the fix
  `command.run` demonstrably executes (netstat sees the listener
  through the seeded python server).
- Probe conventions recorded: `cmux send` types raw keystrokes
  without Enter (upstream parity), so probes wanting execution append
  CR explicitly; netstat row parse requires 5 cols with `:port` in
  cols[1] and `LISTENING` in cols[3] (headers localize, rows do not);
  the owner pid is killed with `taskkill /PID` (never by image name;
  never msedgewebview2); segment checks use tol 4-10 on the lane-3
  inks (#11131a vs #0c0e14 stay tolerance-bound).
- Titlebar-2b probe brought current with the strip's lane-3 geometry:
  the command bar occupies the strip middle, so the blank-strip fill
  sample and the caption dbl-clicks anchor at a fixed 300pt (past the
  chip's worst-case 277pt, before the bar's >=335pt left edge). Visual
  13/13 + behavior 13/13 on the lane-4 binary; first post-fix behavior
  run read 12/1 with the left-edge resize under-grown 34px for a 60px
  drag - cleared on rerun (variance family, ledger).
- Full battery on the lane-4 binary (both executables rebuilt):
  smoke-cli 49/49, m34 turn PASS, M4 16/16, M4.2 13/13, M4.3 19/19,
  M4.4 8/8, restore 51/51, SSH 28/28, m51 99/99 (first try, no
  historical timeout abort), a11y 3 pass / 4 standing skips / 0 fail,
  titlebar-2b 13+13, lane-3 probe 17/17, lane-4 probe 24/24,
  workspace `cargo test` 335 (was 324: +10 port_state, +1 CR
  regression test), fmt + clippy -D warnings clean, `cargo build`
  finished.

## 2026-10-26 - lane 5 (agent activity stream pane)

- New battery live probe `port/harness/test_agent_activity_lane5.py`
  — **36/36 twice on the clean rebuilt binary**. Passive PrintWindow
  captures only (no clicks/keys against the window; input goes through
  the pipe and the seeded pane's own shell), sampling on strides
  (full-band scans cost about 0.7M px per call and would miss the
  about-500ms pending window; ring predicates use tighter strides).
  Environment-isolated app launch, no listening port.
- Phases (one seeded terminal pane beside the agent pane so the event
  paths are all real):
  - open: `cmux agent open --json` splits the agent pane right of the
    focused surface; the reply carries surface/pane/workspace ids,
    `placement: split`, `type: agent`; `system.identify` and
    `surface.list` label it `agent`.
  - measured-geometry bands (probe anchored to an instrumented paint
    print, since removed): agent leaf `[[564,64]..[900,536]]`pt at the
    default window (right split about 37% of client width); app chrome
    bands = title strip 0-32pt + M2.4b pane-tab strip 32-64pt, the
    pane's own tab 64-84pt, stream fill from about 90pt, leaf bottom
    24pt above the client bottom. Band: x0=0.64·cwidth, x1=-4,
    y0=90pt, y1=cheight-24·s-2; tab run scans 66-84pt. Outside this
    band the app's near-token chrome inks (#191b22-class fills) give
    false card baselines - lesson: geometry tables come from live
    measurement, not point arithmetic.
  - stream model: empty-stream baseline (app-side instrumentation
    during debugging verified `CardStore` empty at fresh launch, then
    removed), hook seeding through the REAL admission queue
    (`cmux hooks enqueue claude notification|feed`, payload on stdin,
    `CMUX_SURFACE_ID` pinning the workspace; the CLI fails open, exit
    0 with `{}`, so the probe asserts pixels - never CLI output);
    card fill `#191b22`, headline ink `#e2e2eb`, time ink `#87929a`;
    `feed` behaves as a journal route (card, no ring/toast).
  - the full m34 turn boundary in ONE python invocation typed into the
    pane's pwsh (`cmux send` the text, one `"\r"` submits): arm (the
    Ollama prompt line + CRLF), then `>>> Return FINAL_OLLAMA_OK` +
    the marker, then the bare prompt line held while the process
    sleeps about 11 s — the pending window opens for about 500 ms
    (ring visible 0.5-0.7 s): accent `#ffc42f` ring during pending,
    ring gone at delivery, delivered cards `n3 > n2 + 2` for the +/-1
    sampling noise. Lesson: two separately typed commands leave pwsh
    typed-line echo state noise and never fire — the earlier
    debugging era's silent failures all trace to that split.
  - close semantics: the seeded terminal pane exits; the agent pane
    keeps the workspace alive (no child process — no exit cascade)
    and refuses text input; the plain-mode grammar is asserted here,
    where a second uncloseable pane no longer disturbs counting.
  - relaunch round trip: the agent pane restores with the ORIGINAL
    id, its tab repaints, and the stream comes back empty — cards are
    not persisted (the same truthfulness a new unit test pins on the
    snapshot save path).
- Titlebar-2b probe fix (harness-side; no app change): a launch from
  a background caller (and SW_RESTORE from minimize) leaves a
  full-screen maximized foreground overlay eating every injected
  click — the close-button phase failed on this, not on the app.
  `test_titlebar_2b.py` gained `pin_foreground(hwnd)` (ALT-trick +
  retry) at window discovery and again after restore; behavior 13/13
  on rerun. A standalone diagnostic confirmed the app's own close
  path exits cleanly with the session snapshot saved.
- Temporary diagnostics KEPT for owner disposal (AGENTS.md §3
  no-delete rule): `port/harness/probe_agent_diag.py` and
  `port/harness/probe_titlebar_diag.py`.
- Gates + full battery on the lane-5 binary: fmt + clippy
  `-D warnings` clean, workspace `cargo test` 366 (was 335), `cargo
  build` finished; smoke-cli 49/49, m34 turn PASS,
  M4.1 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, restore 51/51, SSH
  28/28, m51 99/99, a11y 3 pass / 4 standing accesskit skips / 0
  fail, titlebar-2b visual 13/13 + behavior 13/13 (after the
  foreground pin), lane-3 17/17, lane-4 24/24, lane-5 36/36.

## 2026-10-26 - design-fidelity round 1 (theme ladder + sidebar width + title/status folds)

- Owner-triggered fix round ("the design is not identical at all the
  color the responsive"): root cause and the six corrections are
  recorded in figma-design-reference.md sec. 14 — chrome split into
  two schemes while the owner's legacy `theme.background #1e1e1e`
  block was configured; aside 216pt vs the design's 256 (node 1:399);
  PowerShell 5.1's OSC image-path title overflowing every title
  consumer; status-bar marker and SID-pipe-leaf collisions.
- Unit additions to `port/crates/cmux-shell/src/main.rs` (+4 →
  workspace 370): `friendly_osc_title` pins (absolute drive/UNC path
  ending `.exe` case-insensitive → image stem; relative/bare/
  non-image titles and empty stems pass through), the
  `STATUS_MARKER_MAX_TITLE_CHARS` cap test (head + ellipsis; the
  focused `*` counts on top; exactly-at-cap whole),
  `shade_lighter_rounds_channels_upward_and_clamps` (inverse of
  `shade_darker`; design ladder `#191b22 → #1c1f27` check), and
  IPC-leaf fold cases inside the existing claude/ipc/version test
  (SID-derived pipe → `pipe: …09267108-1001`; at-cap whole; one-over
  → ellipsis + last 13). Theme tests extended in place (defaults pin
  `chip_background`, the config-mapped chip, the
  unknown-fallback chip, the configured `0x1e1e1e` case).
- Gates, all green on the final binary: fmt 0, clippy
  `-D warnings` 0, workspace `cargo test` 370/0, `cargo build`
  finished. Re-applied lesson (M5.2 era): the exe locks while the
  app runs — build fails `os error 5` until the running instance
  exits cleanly (WM_CLOSE / `CloseMainWindow()`; never taskkill).
- Probe re-anchoring for the 256-wide aside (geometry tables move to
  the design node + live checks, never stale measurements):
  `test_sidebar_services_lane4.py` (`fill_x1=int(254·s)`, badge zone
  `(256-46..256-6)·s`, "256pt column" wording),
  `test_agent_activity_lane5.py` (agent leaf
  `x0=((cwidth_pt+256+12)/2)·s` — the balanced 2-leaf split with the
  12pt gutter; tab run anchored to the leaf edge),
  `test_terminal_ux_m43.py` (`SIDE=256`), `test_browser_m51.py`
  (`sidebar_px=round(256·scale)`). `probe_agent_diag.py` stays
  stale-on-purpose (temp, owner disposal).
- Flake ledger across the round (profile, not fixed checks):
  titlebar-2b behavior flaked 12/1 once and 8/5 once — cleared to
  13/13 on the FIRST rerun both times (launch-timing/foreground
  overlay, the recorded profile); m43 flaked 15/4 once on the same
  binary and cleared (19/19). Final same-binary pass:
  titlebar-2b visual 13/13 + behavior 13/13, lane-3 17/17, lane-4
  24/24 twice, m43 19/19, m51 99/99, lane-5 36/36 — every member
  green on the corrected build.
- Live verification with the OWNER config in effect (plain
  Start-Process of the built exe, no env overrides): PrintWindow
  captures `cmux_live_1.png` (pre-fix) / `_2.png` (mid-fix) /
  `_3.png` (final) plus zoom crops `cz*.png` under
  `%TEMP%\opencode`. After-state: strip/sub-header/sidebar/gutters
  `#1a1a1a`, pane interior/status bar/chips `#1e1e1e`, ring
  `#38bdf8` at x=320px (the 256pt edge), breadcrumb
  `cmux / powershell › powershell`,
  `1:powershell* | CPU: 30% MEM: 11 GB CLAUDE: IDLE` + folded
  `pipe: …09267108-1001` + `UTF-8` + `cmux-win11`, zero collisions.
  The `_2.png` mid-state motivated the IPC-leaf fold: the
  SID-length pipe label interleaved with `CLAUDE: IDLE` (gap-column
  scan found no separation).
- Owner decisions still open (recorded, not actioned): dropping the
  legacy theme block from their personal `cmux.json` for the exact
  design palette (their file — untouched by the port); disposal of
  the temp diagnostics (`probe_agent_diag.py`,
  `probe_titlebar_diag.py`, capture helpers in `%TEMP%\opencode`);
  commit authorization (everything uncommitted).

## 2026-10-26 - design-fidelity round 2 (sub-header edge + drawn x + theme drop)

- Owner directive ("do as figma") plus the two besides-bug reports
  (weird close symbol, "missing" +). Verified root causes
  (figma-design-reference.md sec. 15): the sub-header strip painted
  from the window edge and covered the aside's + row (the code always
  painted it — pixel scan of `cmux_live_3.png`: zero ink in the +
  band), and the close glyph's source literal carried mojibake bytes
  (`C3 83 E2 80 94`) — the byte audit is ground truth where the
  cp1252 console output had confounded earlier greps.
- Fixes (`main.rs`): sidebar computed before `subheader_rect` and the
  strip starts at `sidebar_rect.right()`; close glyph = two
  `painter.line_segment` strokes (font/encoding-independent);
  owner-authorized legacy theme block removed from their `cmux.json`
  (file otherwise byte-identical). No new units — workspace
  `cargo test` stays 370/0; fmt + clippy `-D warnings` clean; build
  on the closed-app path.
- Probe re-anchor: `test_subheader_lane3.py` left-chip scans start at
  the sidebar edge (`round(256·s)`) with 0.72/0.80·cwidth windows
  after the strip moved right (design-true band fill read:
  `#191b22`).
- Battery on the round-2 binary, all first-run green (zero flakes):
  titlebar-2b visual 13/13 + behavior 13/13, lane-3 17/17, lane-4
  24/24 twice, m43 19/19, lane-5 36/36, m51 99/99.
- NEW behavioral proof `plus_click_probe.py` (temp, owner disposal):
  fixture-env instance; the 2b pin recipe only works when the
  foreground pin is VERIFIED (retry loop until `GetForegroundWindow`
  sticks) + `ClientToScreen` mapping + a `WindowFromPoint`
  app-owner guard; real mouse down/up at the + row center → IPC
  `workspace.list` before=1 after=2, graceful WM_CLOSE teardown.
  The earlier no-op runs (SendInput AND PostMessageW) traced to the
  probe's unverified pin — same-binary 2b behavior 13/13 shows click
  delivery itself is sound. Lesson recorded: injected-click probes
  must verify the pin exactly like 2b.
- Live palette acceptance (owner env, `cmux_live_4.png` vs `_3.png`
  under `%TEMP%\opencode`): every chrome token design-exact —
  sidebar `#191b22`, chips `#1d1f26`, title strip + command bar
  `#11131a`, pane interior + status-bar fill `#0c0e14`, ring
  `#38bdf8` at x=320px, sub-header fill `#191b22`; + ink 18 px at pt
  center (128,53); close strokes 24 px (y 104..115). New temp
  artifacts for owner disposal: `plus_click_probe.py`,
  `ascii_audit.py`, captures `cmux_live_4.png` (plus zoom crops
  `close_glyph.png`, `plus_zone.png`, `rows_top.png`, `ws_strip.png`).
- Mojibake inventory: comment-layer only (about 90 sites, valid
  UTF-8, cosmetic — em-dash/quote/arrow/Ã encodings inside doc
  comments and banners) — recorded for a later normalization pass,
  out of scope, zero UI strings affected.
- Owner decision CLOSED this round: the theme block was dropped
  (open in the round-1 record). Still open: M6 MSI/signing assets,
  disposal of the temp scripts (now including the two new ones),
  commit authorization (everything uncommitted).

## 2026-10-26 - round 3 (status-bar telemetry font + version display removal)

- Owner asks for the bottom furniture; both diverge from the mock by
  owner decision (figma-design-reference.md sec. 16): the CPU/MEM +
  agent telemetry run paints 10px (pane markers stay 12px), and the
  aside's bottom version strip (node 1:485 — fill + hairline + the
  literal `v`+version chip) is no longer drawn; its 38pt budget
  released to the rows (bounds now 6pt above the aside bottom).
- Code: `telemetry_font` in `draw_status_bar` (measured with the same
  font it paints — the round-1 hard-reserve semantics keep the
  telemetry collision-free; overflow markers still fold first);
  `SIDEBAR_BOTTOM_STRIP` + `status_version_label` removed with their
  unit pin and the strip's paint block.
- Method note: no harness probe sampled the removed band or the
  telemetry glyphs (grep audit before the change) — no assertion
  re-anchoring needed. Verification is pixel-based on the live build
  (`cmux_live_5.png` vs `_4.png` under `%TEMP%\opencode`):
  aside-bottom band `#1d1f26` + 180 ink px → plain `#191b22` + 0 ink;
  the identical `CLAUDE: IDLE` block 11px → 9px tall; telemetry run
  right edge 703px → 660px; UTF-8/product-chip zones unchanged.
- Gates on the round-3 binary: fmt 0, clippy `-D warnings` 0,
  workspace tests 370/0, build on the cleanly closed app (WM_CLOSE
  only). Battery re-run, all first-run green (zero flakes):
  titlebar-2b visual 13/13 + behavior 13/13, lane-3 17/17, lane-4
  24/24, m43 19/19, lane-5 36/36, m51 99/99.

## 2026-10-26 - round 4 (sub-header band removal: absence probe rewrite)

- Owner ask: remove the whole lane 3 chip band (git/diff/agent chips,
  keycap pills, split tray). The lane-3 probe's 8 band-PRESENCE
  checks became meaningless with the band, so the file
  (test_subheader_lane3.py) was rewritten as a band-ABSENCE probe —
  not deleted — keeping its launch/window/capture/key scaffolding:
  visual A1-A4 (exact-match absence of `#191b22` band fill,
  `#1d1f26` chip fill, violet ink, `#11131a` tray fill in y 32..64pt
  right of the aside) + R1/R2 (the pane surface `#0c0e14` owns the
  region; the focused pane's `#38BDF8` ring rides the pane top edge)
  + the intact command-bar checks; behavior keeps the palette flow
  (bar click + Ctrl+K + arrows + Enter marker readback) and swaps
  the tray-click split for the keymap `ctrl+shift+r` split + gutter.
  Visual 15/15, behavior 22/22 (includes 3 honest foreground-pin
  checks).
- Two lessons hardened into the record: (1) absence predicates must
  match the removed flat fills EXACTLY — a ±4 tolerance caught
  prompt-glyph AA neighbors (#1c1e24/#1d1f24) and produced
  deterministic false-absence failures twice before the diag script
  pinpointed it; (2) the bar-click trio's failures were the probe's
  own unverified foreground pin AGAIN (WindowsTerminal overlapped the
  probe instance; WindowFromPoint hit its window) — the m34/2b
  ALT-trick `pin_foreground` recipe is now ported verbatim into the
  lane-3 probe (the app was never at fault; Ctrl+K + keymap split
  stayed green throughout).
- Anchor sweep miss confessed and closed: lane-5's pre-round-4 chrome
  height hid in `90*s` / `66..84*s` forms (the audit grepped only
  32/64/72·s literals), so lane-5 ran 34/2 once until
  `agent_band`/`tab_run` re-anchored (leaf top 32pt now); re-ran
  36/36. mA43/m51 had exactly the one 64pt chrome anchor each and
  were re-anchored in the edit pass (`32 * s`).
- Gates on the round-4 binary: fmt 0, clippy `-D warnings` 0,
  workspace tests 369/0 (round-3 total 370 minus the retired keycap
  unit pin), build on the cleanly closed app (WM_CLOSE). Battery,
  final runs all green: titlebar-2b 13/13 + 13/13, lane-3 15/15 +
  22/22, lane-4 24/24, m43 19/19, lane-5 36/36, m51 99/99. Fixture
  note: the lane-3 git fixture (`make_git_fixture`) left with the
  chips it lit up.

## 2026-10-26 - round 5 (design side-by-side closeout; no probe changes)

- The owner's "finish all the design rounds" closed the parked Figma
  side-by-side. The battery stays untouched; the comparison ran as a
  one-shot analysis against the recorded design spec
  (figma-design-reference.md secs 1-17) on the fresh capture
  cmux_live_7.png: 23 checks / 23 pass, 0 fail, with the three
  owner-directed divergences classified EXPECTED (band absent, aside
  version strip absent, cmux-win11 status chip kept). Drift
  attribution vs cmux_live_6.png shows session-state deltas only
  (the active workspace row's chip position and the CPU/MEM digits);
  title strip and terminal zones byte-identical (0 diff px). Records:
  figma-design-reference.md sec 18.
- Predicate lesson (no probe change needed): at this DPI the ring's
  horizontal edges paint subpixel-AA cyan-family pixels, not exact
  #38bdf8 - the standing battery predicate (BGR B>200 / 120<G<220 /
  R<120) is the correct rule; the y=41 top edge reads 797 family px,
  exact-only scans read false absence (this session's diagnosis
  scripts prove it; the battery probes were already correct).
- Honest disclosure: the Figma MCP file reads stayed paywalled on the
  Starter/View-seat plan this month (create_session OK, get_metadata
  banner). The comparison therefore runs against the verified node
  reads of secs 1-17, which the round-1..4 lanes pixel-validated.

## 2026-10-26 - round 6 (cmux-win11 chip removed; workspace identity segment)

- The owner answered both open items in one message: "okay remove it"
  (the cmux-win11 status chip) and the blue focused marker must
  visibly change with the current running workspace ("you remove it or
  match it"). The chip is gone from `draw_status_bar` and the UTF-8 +
  `pipe:` leaf segments re-anchored to the strip's right pad; the
  strip now leads with the active workspace's identity segment
  `w1 · powershell` (secondary ink + a muted separator) before the
  marker run - the minted `w{N}` id is the visible switch tracker
  because the live-derived name alone folds to `powershell` in every
  workspace (the exact complaint's shape). The a11y strip label rides
  the painted label. Records: figma-design-reference.md sec 19.
- Gates on the round-6 binary: fmt 0, clippy `-D warnings` 0,
  workspace tests 372/0 UNPIPED (round-5 total 369 + 3 new pins -
  `fold_label` head+ellipsis, `status_workspace_label` id + name cap,
  `status_ws_fit_case` four-case matrix - plus the exact-label pin
  extended to both leaf variants), build on the cleanly closed app
  (WM_CLOSE of the owner instance that locked the exe). The release
  lane was attempted per battery default and still stops in
  `libghostty-vt-sys 0.2.2` build.rs ("failed to execute zig build:
  program not found"; the zig toolchain is absent - pre-existing,
  unrelated to this round), so the battery runs the debug binary via
  `--bin` (the idiom every battery round here has used).
- Behavior probe (new, %TEMP%\opencode\round6_ws_probe.py, fixture
  instance on an isolated pipe): 14 checks / 14 pass, 0 fail on the
  final binary. `workspace.create` mints `w2` and activates it
  (focus:true verified through `workspace.current`), captures A/B
  bracket the switch: chip-absence in both captures (fill-run rules,
  below), the identity zone x 322..380 differs on switch (61 px), the
  identity label paints secondary ink (238 px), the right cluster's
  muted ink stays re-anchored, the focused marker keeps blue ink, and
  teardown exits cleanly via WM_CLOSE. Two intermediate runs drove the
  probe's own predicate fixes (zero-exact-px chip asserts were
  over-strict against glyph AA; a mode-less invocation quirk in one
  battery probe is noted below, not here).
- Battery subset on the round-6 debug binary, all first-run green,
  zero flakes: m43 19/19; a11y-uia 3 pass / 4 standing accesskit
  skips / 0 fail (the documented standing state); titlebar-2b visual
  13/13 + behavior 13/13; lane-3 visual 15/15 + behavior 22/22 (a
  mode-less first run prints only the behavior half - both mode args
  exercised); lane-4 24/24; lane-5 36/36; m51 99/99.
- Live verification (owner relaunch, captures into %TEMP%\opencode):
  cmux_live_8.png; the strip reads
  `w1 · powershell | 1:powershell* | CPU: 26% MEM: 10 GB CLAUDE: IDLE
  … pipe: …09267108-1001 UTF-8`; the focused marker's strict-info
  blue restores at x 465..554 (186 px - the same count as live7's
  332..421 run, shifted right by the ws segment's span). The round-6
  side-by-side scanner (sidebyside_r6.py; sidebyside_final_r5.py
  stays untouched as the round-5 record) passes 24 checks / 0 fail -
  S16R chip-absence (fill block + bbox runs), S-WS identity ink
  (264 px in x 324..470), S19R marker blue in its moved zone, D3R the
  removal classified EXPECTED - and the drift attribution
  live7→live8 = 6524 px, ALL in the status bar (aside, title strip,
  and terminal zones zero).
- Lesson - helper-vs-paint consistency: the fold matrix must solve the
  same inequality the paint pass applies. The first live binary still
  subtracted `ws_span` inside `markers_max_x` while the fit loop's
  cursor already started past the painted ws segment (the r5 formula
  shape, where the cursor had no ws term yet): the span counted twice
  and one-pane marker runs fell behind a bare ellipsis while the full
  identity label painted. Diagnosed with a temporary `strip-fit`
  eprintln through a fixture instance and the owner session (stderr
  redirect idiom), fixed (`markers_max_x = right_clamp - 10 -
  telemetry_span`, with the reason as a permanent comment), eprintln
  removed before the gate rerun; probe + battery + live all green on
  the final binary.
- Lesson - chip-absence pixels: the re-anchored leaf text now lives
  inside the old chip bbox, and text glyph AA there hits the exact
  fill color in isolated single px (verified at (524,685)/(663,680),
  identical across captures) - absence predicates are fill-RUN rules
  (no horizontal run >= 6 px of exact #191b22, no fill-heavy row with
  > 5 px), not zero exact-px counts.

## 2026-10-26 - round 7 (whole-application palette audit; browser default background)

- The owner's clarified complaint pointed at the WHOLE application vs
  the figma file, so the probe asserts moved from widget zones to a
  full-window census (cmux_live_8.png, re-checked on cmux_live_9.png
  post-fix): the four sec-2 base tokens measure 66.02 / 27.24 / 1.91
  / 1.64% and every other pixel classifies as hairline, edge, or
  glyph AA. The terminal zone (99.27% #0c0e14; prompt text pure
  #ffffff at x 334..482 y 49..64) pins the egui_tty emulator-default
  cell colors as a recorded owner-gated divergence candidate (the
  design's sec 3 pane-body text is an illustrative mockup, not a
  cell spec).
- Palette overlay predicates: the overlay opens by a CLICK on the
  command bar (GetWindowRect + offset left+680, top+20); SendInput
  Ctrl+K is unreliable at this DPI (the T0-vs-P control capture
  differs by 96 px - the status bar's CPU digits - so keystroke
  probes cannot distinguish the palette from noise). Census asserts:
  fill run #11131a (23610 px), selected row #1d1f26 (13730 px),
  region token-family share >= 0.99.
- New canvas ladder probe (round7_canvas_probe.py, fixture instance
  on an isolated pipe): about:blank leaf #121212 240,478 px,
  byte-identical pre/post fix; plain no-CSS page #0c0e14 297,721 px
  with browser.eval computed html/body both rgba(0, 0, 0, 0)
  (proves the WebView2 default-background mechanism is live and the
  token paints under undefined-background pages); explicit token
  page #0c0e14 298,313 px; green disambiguator #40ff80 240,377 px
  (proves Chromium content renders into PrintWindow captures - so
  the blank #121212 is Chromium's own base paint, not app paint).
  round7_theme_probe.py regained fixture retention: the app-stderr
  log is kept and scanned for cmux-webview2 lines (the options path
  executed with zero errors on Evergreen 156.0.4314.42; no rmtree).
- Code verification artifacts: the cmux-webview2 goldens grew a
  unit pin (`corewebview2_color` token pack + warm-byte order
  guard); workspace tests 373/0 UNPIPED (24 suites), fmt 0, clippy
  -D warnings 0 (exit codes verified), build 0. The m51 browser
  battery re-ran against the rebuilt binary: `smoke complete: 99
  passed, 0 failed`, exit 0 - its battery row is the regression line
  for the BrowserSpec field addition (the spec grew
  `default_background`; the single call site threads the resolved
  viewport fill).
- Figma MCP record (this round's single attempt): create_session
  succeeded; get_screenshot AND get_metadata both stop at the
  Starter-plan rate-limit paywall. Standing rule stays: at most one
  MCP attempt per round; probes compare against the verified node
  reads, never the live file, until the owner upgrades or unblocks.
- Owner-gated residuals recorded (not probe work): the about:blank
  gray (keep upstream landing vs token-painted default data URL),
  terminal cell re-theming, and the chrome typeface choice.

## 2026-10-27 - round 8 (pane markers retired; recommended residuals executed)

- Two owner directives in one message: remove `1:powershell*` from
  the status bar and keep going, plus "for my decision just do the
  recommend". The marker run (`ordinal:title`, `*` focused) left
  `draw_status_bar` with its helper, cap, fit term, and two unit
  pins; the strip is ws identity | CPU/MEM/CLAUDE telemetry | pipe
  leaf + UTF-8, and the a11y label still counts the workspace's real
  panes. Probe consequence: round6_ws_probe.py's focused-marker
  assertion rewrote to marker-retirement rules - a strict info-blue
  shape predicate (blue >= green+20, blue >= red+60, blue >= 165)
  because the round-6 tol-90 near-box ALSO matched the identity
  label's secondary ink (the live repro: 233 px of false positives
  from the ws label under the loose box), plus a "no secondary ink
  in the former marker zone" check. The probe passed 15/15 on the
  round-8 binary; two capture-diff numbers note the strip's new
  shape (identity-zone switch diff 61 px = the whole strip diff
  now, vs 117 px when markers still rode along).
- The recommended residual became code: browser_home_url()
  (title-less, content-less data URL; body background = the resolved
  viewport fill, percent-encoded, rederiving on every call so a
  configured theme.background moves the home) - browser.open_split's
  default landing. Restored leaves keep their saved URL (old
  about:blank leaves stay as saved). Unit pins: the token-encoding
  identity, a warm-token reorder guard (byte order through the
  format), and the data-URL scheme. Recorded upstream divergence:
  upstream derives its default page from browser preferences (M5.1).
- Gates on the round-8 binary: fmt 0, clippy `-D warnings` 0,
  workspace tests 372/0 UNPIPED (24 suites; two marker pins
  retired, one home pin added - the 373->372 delta reconciled),
  build 0; m51 battery re-ran on the rebuilt binary:
  `smoke complete: 99 passed, 0 failed`, exit 0 - its open/default
  page/restart legs now exercise the token home end to end.
- Live: cmux_live_11.png census on-token 97.02%; strip info-blue
  0 px; identity ink 238 px; drift vs cmux_live_10.png (2305 px) is
  entirely inside the strip band - the removed marker zone plus the
  telemetry's left shift, the rest of the window byte-identical.
  Terminal cells + chrome typeface recorded keeps (the design does
  not spec cells; the mock faces would re-break rounds 1-6's tuned
  fold widths for one face-swap).

## 2026-10-27 - round 9 (sidebar depth: the strip's scroll/reorder/rename battery)

- New battery `port/harness/test_sidebar_depth_r9.py`, **35/35 on the
  final binary** (single post-cleanup run; one twice-checked earlier
  run also 35/35 on the instrumented one). Two legs: leg 1 seeds 12
  workspaces (overrides a 640x400 window's 242pt strip viewport),
  pins the scroll state via IPC select (the app's reveal math) and
  exercises the wheel round trip + terminal no-steal, an exact
  click-drag reorder (row 1 → row 3's lower half; planner slot 4),
  a scrolled drag whose selection must follow by id, a rename commit
  (`Ctrl+A` + retype + Enter → `customTitle`) and a one-press
  Escape cancel; leg 2 relaunches on the saved session and asserts
  order, rename-on-the-right-id, and the restored active index.
- Pinned-state architecture (the probe's load-bearing decision):
  phase preconditions pin states with `workspace.select` IPC calls
  whose reveal side effects are app-defined (`select(0)` → scroll
  exactly 0, `select(11)` → bottom reveal = content 382 - viewport
  242 = 140), so pixel scroll sensing is GONE: the strip's chip
  `#1d1f26` differs from the sidebar bg `#191b22` by 4/255 per
  channel and only the active row paints - a scanned sensor reads
  noise. `strip_scroll_pt` was written, instrumented (column dumps,
  an ASCII rect map, a full-capture PNG aid), root-caused as
  unreliable, and deleted.
- Recorded instrument facts (verified against the app's env-gated
  wheel log, removed after the round): SendInput `mouseData` +
  120/tick = wheel-UP = egui dy positive = scroll decreases; one
  wheel notch exercises ~12.75pt of smoothed scroll drained over
  ~10-40 frames (single settles can race the drain - the up-return
  check polls the band diff to convergence instead); `PrintWindow`
  needs `PW_CLIENTONLY|PW_RENDERFULLCONTENT` or the caption shifts
  every client pixel (the fixture's earlier numbers were all
  caption-shifted); a HOVERED INACTIVE row paints a soft hairline
  fill, so gesture probes park the cursor off the strip before any
  byte comparison (this produced the round's one false failure);
  the reveal-on-commit for a bottom-anchored active row re-pins
  scroll at exactly 140 mid-drag (the app's reveal math, not a
  scroll leak), and the drag autoscroll's margin crossing is what
  resolved the phase-D release slot.
- Regression ladder on the same binary: lane-4 24/24, m43 19/19,
  m51 99/99, restore 51/51 - the round-9 strip changes (fade mesh,
  scroll column, hover fill) left the scroll-0 lane bands and the
  other batteries' geometry untouched. Note: round 8's docs
  reference `round6_ws_probe.py`, which is ABSENT from the
  workspace (likely owner-disposed with the other round-6 temp
  probes); the r9 battery is the strip probe of record.
- Gates: fmt 0, clippy `-D warnings` 0, workspace `cargo test`
  386/0 (24 suites; sidebar.rs module +10, shell main.rs +4: the
  reorder-move/follow pin, custom-title naming-precedence,
  snapshot round trip, unbound-action resolution), build 0 on the
  debug binary (the release lane still needs zig, recorded).


