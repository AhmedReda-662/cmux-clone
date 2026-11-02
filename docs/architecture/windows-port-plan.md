# Windows port plan — cmux → Rust

Status: proposal for owner approval (Phase 7 report references this file).
Evidence base: `repository-map.md`, `feature-parity.md`, `rea-findings.md`,
and the ADRs in `decisions/`. Anything not yet verified locally is marked.

## 1. Architecture alternatives

### A. egui/eframe + `egui_tty` widget on `libghostty-vt` + `portable-pty` (recommended for the slice)

- `egui_tty` (MIT, crates.io) is a terminal widget drawing directly on
  `libghostty-vt` — the same engine class cmux's own Rust stack binds. It
  already solves the ConPTY hazard of unanswered startup probes (`\e[6n`) via
  an `on_pty_write` reply hook, and ships selection, scrollback search, OSC 8
  links, mouse reporting, bracketed paste, Kitty keyboard protocol.
- Proven Windows ingredients around it: NabiTerm (egui/eframe + wgpu DX12→
  Vulkan→GL incl. Mesa software fallback + portable-pty ConPTY, shipped),
  multiple multipane egui terminal products.
- License chain at PoC: egui MIT/Apache-2.0, egui_tty MIT, `libghostty-vt`
  crate MIT-licensed Ghostty VT (needs per-crate confirmation), portable-pty
  MIT. No GPL exposure if built from crates.io packages.
- Cost: egui is immediate-mode; a sidebar/docking layer (`egui_dock`) covers
  vertical tabs/splits; text rendering quality/DPI/IME must be verified on
  Windows (explicit PoC step).

### B. GPUI + custom `libghostty-vt` rendering (scale-up alternative)

- GPUI (Zed) is now on crates.io (`gpui` + `gpui_platform`; Windows = Win32 +
  DirectWrite, no extra features). Two independent Windows products prove the
  combination GPUI + ConPTY + ghostty-vt: Paneflow (statically linked
  `libghostty-vt` as default Windows engine since v0.8.1) and ZWG Terminal.
- Better long-term feel (DirectWrite text), heavier near-term cost: gpui is
  pre-1.0 with breaking changes; more custom rendering work for the grid.
- Kept as the documented scale-up path; does not invalidate A if PTY and
  emulation stay behind the same traits.

### C. Full `libghostty` + native shell (wintty-style) — rejected for this port

- wintty proves the full engine on Windows (C#/WinUI 3 shell, DX12 renderer,
  P/Invoke `ghostty.dll`), but: not Rust, full libghostty DLL support on
  Windows is recent/"greenfield", its C API is explicitly "young and
  unversioned", and the OSS tier requires Windows 11 + DX12 with no software
  fallback. This is the *reference* for what full-embedding costs, not a
  choice for us.

**Recommendation:** Start with A for the vertical slice, keep B as the
scale-up path, with PTY (ADR-0003) and emulation (ADR-0002) sealed behind
internal traits so neither choice poisons the architecture. Requires owner
approval (manifest §7) before implementation.

## 2. Proposed Rust workspace structure

```
port/
  Cargo.toml                    # workspace: resolver=3, shared profile/lints
  rust-toolchain.toml           # pin channel (stable; MSVC)
  crates/
    cmux-port-shell/            # bin: window + UI loop + sidebar/splits host
    cmux-pty-port/              # PTY edge: portable-pty(ConPTY), commands, job-object cleanup, traits
    cmux-emu/                   # emulator edge: TerminalEmulator trait; backends: libghostty-vt, alacritty_terminal (fallback)
    cmux-layout/               # pane tree + split geometry (port of CmuxPanes/Geometry math + equalize)
    cmux-notifications/         # OSC ingest state, unread/ring models, toast delivery (feature-flagged off in MVP)
    cmux-ipc/                   # v2 JSON-lines over named pipes (auth modes), protocol types
    cmux-cli/                   # `cmux` CLI speaking cmux-ipc (contract: cli-contract.md subset)
    cmux-config/                # cmux.json JSONC store + theme parsing
    cmux-testsupport/           # PTY stream replays, harness helpers
  docs/ -> ../docs              # documentation lives at repo root
```

Seams (from manifest §7): UI/windowing | state/workspace orchestration |
process/PTY | emulation | rendering pane | notifications | browser | CLI/IPC |
git metadata | config/persistence | Windows services. Each lives in its own
crate with an explicit trait at the OS boundary; no OS types above the edge
crates.

## 3. Terminal rendering proof-of-concept (M1 slice)

Goal (task Phase 6): a real terminal inside a real desktop window. Steps:

1. Toolchain gate: rustup stable-msvc installed (approved), `cargo build`
   succeeds on an empty workspace with clippy/fmt wired.
2. `cmux-emu`: integrate the chosen crate backend (crates.io
   `libghostty-vt`/egui_tty path). If the local Zig build is required and
   fails, apply/deny the uucode-cache workaround from libghostty-rs #9
   before falling back (document in `building-windows.md`).
3. `cmux-pty-port`: open a ConPTY, spawn `pwsh -NoLogo` (fallback `cmd /c
   echo` smoke first), wire reader/writer to the emulator callbacks.
4. Shell UI: 80×24 terminal, resize-on-window (debounded WM sizing →
   `MasterPty::resize` + `Emulator::resize`), scrollback rendering,
   selection/copy/paste, IME text entry.
5. Shutdown discipline: on close — kill child via ChildKiller, wait bounded,
   verify no orphaned `conhost.exe`/descendants (Job Object), clean exit path
   exercised from the process supervisor test.

Slice acceptance criteria (all verified on this machine):
- startup on Windows; shell launch; correct output for prompt + `ls`/`dir`,
  `vim`-style full-screen program rendered correctly; Unicode + box-drawing
  text correct; resize propagates and reflows; Ctrl+C terminates the
  foreground child; quit terminates cleanly (no zombies); repeatable local
  build via documented commands; `cargo test` green including a PTY stream
  golden-capture test.

Do not proceed to M2 features until every criterion passes on otherwise.
If the preferred engine cannot be integrated: stop, record the blocker +
attempts in `building-windows.md`/`rea-findings.md`, and propose the
fallback in a revised ADR — no silent substitution.

## 4. Dependency and licensing review

| Dependency | Role | License (verify at pin) | Status/verification plan |
| --- | --- | --- | --- |
| portable-pty **0.8.x (pinned)** | ConPTY PTY | MIT | 0.9.0 has a verified `Child::wait()` hang with the writer held (upstream wezterm/wezterm#6783); pinned after a stepwise probe on this machine — see ADR-0003 verification |
| libghostty-vt (crates.io; resolving to 0.2.2 in this build) | emulator | **MIT OR Apache-2.0** (verified via crates.io API, 2026-10-17; repo uzaaft/libghostty-rs) | **Verified buildable on this machine** with Zig 0.15.2 (not 0.16 — that pin belongs to upstream cmux-tui's fork binding); Ghostty VT built from source inside cargo's target dir |
| alacritty_terminal | fallback emulator | Apache-2.0 | kept as swap-in backend |
| egui/eframe/egui_tty/egui_dock/wgpu | GUI + terminal widget | MIT OR Apache-2.0 (per crate) | PoC verifies DPI/IME/perf |
| tokio (+ io-util/process) | async runtime | MIT | process/pipe I/O |
| windows / windows-sys | OS APIs (Job Objects, pipes, toasts) | MIT OR Apache-2.0 | per-feature feature-gated |
| tauri-winrt-notification | toasts (Post-MVP) | MIT OR Apache-2.0 | superseded: M3.2b uses the `windows` crate directly (MIT OR Apache-2.0, per-feature gated) |
| webview2-com (Post-MVP) | browser pane + CDP | MIT/Apache (verify) | CDP methods verified on docs.rs |
| cmux-tui crates (ghostty-vt, cmux-pty, rust-agent-screen-detection) | reuse candidates from the reference | **CONFLICT to resolve**: repo LICENSE GPL-3.0-or-later vs workspace Cargo `license = "MIT"`; agent-detector examples are Apache-2.0 with pinned herdr manifests | **Do not import until resolved.** Options: (1) ask upstream to clarify/fix Cargo metadata; (2) treat as GPL-3.0-or-later and accept GPL compliance in the port product (distribution obligations); (3) implement equivalents from the crate *interfaces* (clean-room against Rust docs, not source). Decision needs owner input (manifest §12: escalate, don't guess) |
| BUSL directories (`web/`, `cmux-tui/relays/cloudflare-do/`, …) | server software | BUSL-1.1 | never copied (workspace rule) |
| base64 **0.22** | screenshot PNG decode (cmux-shell only) | **MIT OR Apache-2.0** (verified via crates.io, 2026-10-22) | de-facto standard, dependency-free; decode only, pinned minor |

## 5. Milestones

| M | Scope | Acceptance (verified, not assumed) | Main risks | Status |
| --- | --- | --- | --- | --- |
| M0 | Toolchain + workspace skeleton + fmt/clippy/test CI recipe | `cargo build/test/clippy` green on this machine | none (approval to install rustup) | **done 2026-10-17** — rustc 1.99.1 + zig 0.15.2 installed; build/tests/fmt/clippy green; ConPTY lifecycle truths harvested into ADR-0003/test-strategy |
| M1 | Vertical slice (§3) | every slice criterion passes on this machine | Zig build recipe; ConPTY answers; egui IME/DPI | **accepted 2026-10-17** — machine-verified (§3 checklist) + owner's manual run reported working; exit-state handling added in M1.1 |
| M2 | Workspace/tab/split model + config file + themes (subset) | layout tests ported pass; config JSONC load/save round-trip | re-deriving bonsplit semantics (submodule empty) - use PaneTreeModel/Glossary tests as spec, or fetch the submodule *[un]* | **in progress 2026-10-17** - `cmux-layout` geometry port landed, 27/27 upstream tests pass (ADR-0007); split panes wired into the shell (M2.2, see log); config file landed (M2.3, ADR-0008 - JSONC load/save round-trip, live font/gutter/shell/keybindings); session persistence landed (M2.4a, ADR-0009 - save-on-exit, restore-with-repair, tab lists round-trip); workspace tabs + vertical sidebar landed (M2.4b, ADR-0010 - multi-workspace save/restore, ctrl+1-9 family, hidden workspaces kept alive); themes subset landed (M2.5, ADR-0008 divergence 7 - theme scheme/background/cursor in cmux.json). **All M2 items landed; owner accepted M2 in-session 2026-10-18 (ADRs through 0011 also accepted)** |
| M3 | Notifications + agent hooks + screen detection | OSC-driven ring + toast e2e; two agents' hooks installed | toast AUMID rules; hook config diffing | **in progress 2026-10-19** - M3.1 (OSC 9/777 ingress via a PTY-byte pre-feed scanner + per-workspace unread ring; view = mark-read), M3.2a (upstream's delivery policy - dispositions, 7-flag effects sound/suppress gates - with live app-focus input; see ADR-0011) and M3.2b (real WinRT toasts via the `cmux-toast` crate: per-user AUMID shortcut + `ToastNotificationManager`, best-effort worker thread; the two `notifications.*` config keys wired into the policy) landed; **M3.3 slice A landed 2026-10-19** - direct-CLI hook transport (ADR-0005 Amendment 1): `cmux-ipc` named-pipe server+client (DACL to user SID + ancestry gate + bounded envelopes), `cmux-hooks` (codex installer: merge/prune, canonical trust hashes, marker-block config.toml editor), `cmux-cli` binary `cmux` (`ping`, `hooks enqueue`, `hooks codex install/uninstall`), shell wiring (pipe server -> shared policy pipeline -> ring/toast; pane env injection; focused-surface regression fix; see ADR-0011 Amendment 3); **M3.3b landed 2026-10-19** - Claude settings-document parity (upstream's exact event table, incl. StopFailure/inbox-wait asyncRewake/auto-name/CronCreate guard/PermissionRequest feed; exec-form handlers; `hooks claude inject-settings`; spawn-time `--settings` injection with the `CMUX_CLAUDE_HOOKS_DISABLED` gate; journal-route events don't ring; see ADR-0011 Amendment 4); **M3.3c landed 2026-10-19** - typed-claude PATH shim (shared `claude.cmd` shim dir + `hooks claude launch` wrapper verb with pass-through/exit-code semantics; bare-name pane programs left to the shim; pane PATH routed explicitly after observing portable-pty's default env-block composition; see ADR-0011 Amendment 5); **zero-cost real-claude acceptance PASSED 2026-10-19** (hooks load+fire from `--settings` docs, no trust skip; full pre-API lifecycle through the pipe with real toasts - ADR-0011 Amendment 4 closure note); **M3.4 landed 2026-10-19, closed 2026-10-20 (ADR-0011 M3.4 amendment + 2026-10-20 progress entries)** - prompt-line turn detection ported clean-room from the Swift `PromptLineTurnDetector` (Ollama builtin only; herdr screen-scan stays Post-MVP), 17 upstream tests ported 1:1 + runtime/config/ConPTY-repaint/smoke-handover tests (30 in `turns.rs`; workspace 211 green), `PaneEvent` channel + frame wake + frame-loop deadline sweep + policy delivery, foreground-PID divergence documented (ADR-0011); **acceptance: 12/12 smoke battery PASS 2026-10-20 with on-time delivery (t=1.4-2.3 s) in every run; the earlier instability root-caused to the uncontained pane tree (fixed by per-pane job objects, ADR-0003 amendment) + a stale-done-mark smoke-fixture bug (checker read the log before the delivery)** |
| M4 | IPC server + CLI subset + Python harness against port | upstream harness pattern drives the port's named pipe; protocol tests green | none remaining - slices M4.1-M4.4 all landed; divergences recorded in ADR-0005 Amendments 2-4 (harness client connection shape = divergence accepted 2026-10-21, folds into M5.2) | **M4.1 landed 2026-10-20** (ADR-0005 Amendment 2) - v2 automation subset over the pipe: static `system.ping`/`system.capabilities`, app-plane `system.identify`, `workspace.list/current/select/create`, `surface.list`, `pane.list`, `surface.send_text` (keystrokes), `surface.read_text` (visible grid) executed on the frame loop via the new app request plane (400 ms reply budget, structured timeout envelope); global pane/workspace id mint + restore seeding (fixes the cross-workspace `p1` collision); CLI verbs (`identify`, `workspace`, `surface`, `send`, `read-screen`); stdlib ctypes Python harness `port/harness/cmux.py` + live smoke 16/16 PASS; gates green (223 tests, fmt/clippy clean); M3 battery re-verified 9/9 PASS (variant failures root-caused to pre-existing smoke-fixture defects, not the app); **M4.2 landed 2026-10-20** (ADR-0005 Amendment 3, owner-security-review ratified): auth modes `CMUX_SOCKET_MODE=cmuxOnly|automation|password` (`off`/`CMUX_IPC_DISABLED` disable the server), DPAPI-protected password file `%APPDATA%\cmux\socket-control-password` (sources: explicit `--password` > `CMUX_SOCKET_PASSWORD` env > file; `CMUX_PASSWORD_FILE` relocates), per-connection login handshake (`auth.login` V2 + plain `auth <pw>` V1) with upstream-exact reply texts, `auth_required` pre-dispatch gate, SHA-256 fingerprint rotation re-verification (stale connections refused), non-password `auth.login` ack (`authenticated:true,required:false`) + `ERROR: Unknown command 'auth'` CLI tolerance, CLI global flags `--socket`/`--password`, harness `password=` login wiring + live smoke 13/13 PASS; gates green (238 tests, fmt/clippy clean); M4.1 16/16 + CLI 8/8 + M3 battery 9/9 re-verified; **M4.3 landed 2026-10-20** (ADR-0012 fonts, ADR-0013 pane env) - terminal-UX close-out: live-input battery `port/harness/test_terminal_ux_m43.py` 19/19 PASS (twice on the final binary) over SendInput/WM-injected typing, click-focus, wheel scrollback, drag selection → clipboard, copy/paste, CJK burst, resize hammer, WM_CLOSE + session write; product changes: no forced `TERM`/`COLORTERM` in panes (Ctrl+C breakage under ConPTY root-caused; documented ADR-0013), Windows system-font chain for the grid (ADR-0012, +7 tests = 245); remaining M4 items: multi-instance socket pool (deferred - clients hold connections for ms) | **M4.4 landed 2026-10-20** (ADR-0005 Amendment 4) - the deferred M4 item closed: the pipe name holds 8 concurrent instances, each accepted and served on its own thread (single-instance serialization gone; a parked/silent peer can no longer wedge IPC); name-claim order keeps the second-instance `IPC disabled` path; `HandlerFn`/`PasswordProvider` Arc'd + `Sync`; saturation = OS `ERROR_PIPE_BUSY` + documented client retry (in-band `overloaded` reply text = recorded divergence); verified: +2 pool unit tests (parked request doesn't hold the pipe; 4 concurrent requests id-paired), live pool smoke `port/harness/test_pool_m44.py` 8/8 PASS twice (32 pooled exchanges; CLI ping 0.01-0.05 s while a silent peer holds a slot), gates green (247 tests), all baselines re-run on the final binary (M4.1 16/16, M4.2 13/13, CLI 8/8, M3.4 9/9, M4.3 19/19) |
| M5 | Browser pane (WebView2 + CDP) + automation subset | snapshot/click/type/eval from IPC against a local site | runtime (Edge WebView2) availability checks | **landed 2026-10-22: M5.1-M5.4 (the spec's P0 automation list closed in M5.4)**; per-slice detail - **M5.1 landed 2026-10-21** (ADR-0006 Amendment 1) - WebView2 host in `cmux-webview2` (`BrowserHost`: env→controller→wire lifecycle tagged `HostPhase`, per-frame `tick(TickParams)` pushing bounds/visibility/focused/window-pos, per-surface UDF `%LOCALAPPDATA%\cmux\webview2\<pane-id>`, Failed-phase overlay with the reason) integrated into `cmux-shell` as `PaneBody::Browser(PaneBrowser)`: frame-loop hosting via `sync_browser_surfaces` (hidden panes tick with `visible=false`, bounds `None`), IPC verbs `browser.open_split/navigate/back/forward/reload/url.get` with `not_found`/`invalid_state`/`invalid_params` mapping (back at history start = success no-op), browser facts on `identify` + `surface.list` rows (`surface_type`/`is_browser_surface`/`browser.{url,title}`), keyboard chord interception (`add_AcceleratorKeyPressed` on `ICoreWebView2Controller` → `AccelRouter` marks handled + wakes the frame loop → `ui()` drains and maps digits to `SelectWorkspace` between `process_bindings` and `drain_actions`) verified while the webview child owns native focus, session persistence round trip (`PaneLayoutSnapshot.browserUrl` on the wire, restore spawns a browser leaf with no shell, `browser.navigate` tracks `start_url` so save/restore reopens the live page), and the model-focus rule tightened to egui focus **transitions** (standing terminal-widget focus no longer reverts a browser leaf's model focus every frame — root-caused from run evidence with the new `focus ->` / `ipc serve` diagnostic lines); live smoke `port/harness/test_browser_m51.py` **39/39 PASS** incl. restart section (restored pane reloads the saved page, identify replays the saved focused id from session.json), unit suite 255 (+8: shell 3, webview2 3, ipc 2), all six baselines re-run on the same binary (M4.1 16/16, M4.2 13/13, M4.3 19/19, M3.4 9/9, CLI 8/8, pool 8/8×2), gates fmt / clippy `-D warnings` / test green; **M5.2 residual slice landed 2026-10-21** (ADR-0006 Amendment 2): digit-chord modifier gate landed (`AccelRouter::routes_as_chord` with `GetKeyState(VK_CONTROL)` read inside the COM callback — the accelerator events still fire for plain/alt digits but the gate no longer treats them as chords; smoke negatives: plain digit and alt+digit through the webview never switch, positive: ctrl+2 still routes a chord), model/native focus unification landed (click claims via `render_pane`'s `clicked_now` routed by the unit-tested pure `focus_reconcile_target` — the M5.1 accepted corner is closed; controller GotFocus queue keyed by the new `BrowserSpec.surface_id`, drained per frame and gated to active-workspace leaves, LostFocus deliberately inert; egui standing focus surrenders whenever a browser leaf owns the model, which removes the egui-remount phantom reclaim observed as a `focus -> p1` line right after each workspace re-select in the pre-fix logs), and the smoke's teardown check hardened to exact pid attribution by the per-surface udf path in process command lines (its one earlier count failure was ambient churn against an 11-foreign-process baseline); browser smoke 43/43 PASS (new: gate negatives/positive + udf-attribution checks), unit suite 260 (webview2 +2 gate/focus-drain tests, shell +3 reconcile-corner tests), all baselines re-run on the final binary (M4.1 16/16, M4.2 13/13, M4.3 19/19 after one unrelated SendInput flake retry, M4.4 8/8, CLI 8/8 with the documented retry for the M4.1-era startup-burst race, M3.4 turn runner PASS) | remaining M5.2: superseded (see the 2026-10-22 log entry); **M5.2 CDP automation landed 2026-10-22** (ADR-0006 Amendment 3): the P0 subset `browser.snapshot`/`eval`/`click`/`dblclick`/`type` over WebView2 `CallDevToolsProtocolMethod` through the deferred-reply completion plane (serve validates+shapes+issues the first call and parks `PendingCdp`; the WebView2 completion outbox plus the new `FrameWakeFn` wake the frame loop; `sync_browser_surfaces` drains `take_cdp_completions()` per host and runs the `CdpStep` state machine; click/dblclick issue a fresh-center evaluate, mousePressed, mouseReleased with clickCount 1 or 2; type issues a focus evaluate then `Input.insertText` with the whole-string insert recorded as the divergence from upstream's per-key stream; snapshot mints `@eN` refs in DOM order with re-resolving selectors and any navigation/history verb clears the table; unknown ref = invalid_params, vanished mid-flight = invalid_state, both with the re-snapshot hint; selectors embed as serde_json JSON literals, injection-safe and unit-tested; permanent probe: slow COM ticks >5 ms rate-limited per host + slow CDP completions >100 ms with host-side `CdpTracker` round trips — evidence: lifecycle-moment ticks only, 5.1-17.2 ms, zero slow completions, smoke-measured eval round trips min 10.5 / median 10.7 / max 21.0 ms against the 400 ms budget), browser smoke 60/60 twice on the final binary (the automation section runs on the second instance so fixture navigation never touches the first-exit session file), unit suite 269 (+4 ipc parse, +4 shell automation, +1 webview2 tracker), all baselines re-run on the same binary (M4.1 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, CLI 8/8, M3.4 turn PASS); ADR-0005 Amendment 5 landed the persistent harness client (one pipe per client lifetime, auth once per connection, reset only on transport-level failures) | remaining M5: the fill/press/hover/scroll/get.*/is.*/screenshot/find.*/frame.*/dialog P0 family, CLI wrappers for the automation verbs, history readback snapshot-based, UDF-after-close decision stands (keep);**M5.3 landed 2026-10-22** (ADR-0006 Amendment 4) - the observation + form/scroll family: 22 new IPC methods (APP_METHODS 20 to 42; capabilities advertises the sorted union) - get.title/text/html/value/attr/count/box/styles, is.visible/enabled/checked, fill/focus/check/uncheck/select, hover, scroll/scroll_into_view, screenshot (Page.captureScreenshot viewport PNG staged under the %TEMP% lane with the path in the reply), and shell-side focus_webview/is_webview_focused; every element verb takes BOTH driver flavors (ref XOR selector - the upstream CLI grammar from cmux-main/CLI/cmux.swift ~19460-20135) and echoes the consumed flavor under the same key; browser.eval accepts expression (canonical) or script (upstream's field, CLI/cmux.swift:19444) as siblings of the M5.2 deferred-reply machinery (one uniform one-evaluate ElementJs plane); smoke test_browser_m51.py extended to 87 checks PASS twice, unit suite 282, all six baselines re-run green on the same final binary (M4.1 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, CLI 8/8, M3.4 turn PASS); **M5.4 landed 2026-10-22** (ADR-0006 Amendment 5) - press/keydown/keyup (a Playwright-style key table ~82 rows mapped to Input.dispatchKeyEvent keyDown/rawKeyDown/keyUp with CDP modifier bits; keys drive the focused element; press sends the down+up pair) + wait (the AND of the given predicates, completion-driven re-poll to a deadline, timeout default 5000 cap 120000, per-call reply budget = deadline + 8 s headroom, satisfied replies carry waited_ms and deadline hits the structured timeout problem; APP_METHODS 42 to 46) - smoke extended to 99 checks PASS twice, unit suite 290, all six baselines re-run green on the same final binary |
| M6 | SSH/remote, session restore, custom commands, accessibility pass, packaging | per feature-parity rows | ControlMaster gap; MSI/signing | **in progress 2026-10-22** - warm-up slice landed: the `cmux browser` CLI wrappers over the 37 landed browser.* verbs (ADR-0006 Amendment 6; the wire itself landed in M5.2-M5.4); slice 1 landed 2026-10-22: custom commands (row 14) - config `commands` schema + `command.list`/`command.run` wire verbs + `cmux command` CLI (ADR-0008 Amendment 1); SSH spike grounded 2026-10-23 (ADR-0014 approved, research/ssh-remote-evidence.md); accessibility spike closed 2026-10-23 (ADR-0015: WidgetInfo labels landed; eframe `accesskit` feature reverted pending upstream/environment verification - research/accessibility-evidence.md sec. 6); session-restore verification landed 2026-10-23 (row 22: lane test_restore_m6.py 51/51 PASS twice); SSH implementation slice landed 2026-10-23 (row 20 subset: ADR-0014 amendment 1, lane test_ssh_m6.py 28/28 twice, battery green; repository map records the new verb); **packaging slice landed 2026-11-01** (row 27: ADR-0017's bundled pair rides along; machine MSI built + validated, progress log 2026-11-01) - remaining: Release publication, silent install smoke, signing, CI recipe |

## 7. Progress log (verify-then-write)

- **2026-11-01 - M6 packaging slice closed: the WiX MSI built and validated
  on this machine (the port's first real installer).** Parity row 27;
  bundled-ConPTY ride-along context: ADR-0017.
  - Diagnosis before any build: **no MSI had ever been produced** — the
    release lane exited before linking (plain shells lack zig; the recipe
    is `port/scripts/env.ps1`), no WiX anywhere, no release CI, and the
    GitHub Releases page was empty. The committed wxs additionally failed
    to compile (sign-at-install CustomAction + missing prepvar, wrong
    LICENSE/template paths, unreferenced desktop-shortcut component, a
    machine-wide `CMUX_CONFIG_FILE`). Repo diagnostics: the work
    `.gitignore` was UTF-16 (invisible to git) and the single commit on
    GitHub carried only README/docs/installer-spec files — no app source.
  - WiX **3.14.1.8722 installed portable** (owner-selected option) from
    the official `wix314-binaries.zip` to `C:\Users\noah\tools\wix-3.14`;
    candle/light verified; nothing machine-wide.
  - Release build restored after an Oct-31 user edit had broken
    `cmux-ipc/src/server.rs` (`PSECURITY_DESCRIPTOR` import lost): the
    shared-descriptor design is preserved via a
    `SharedSecurityDescriptor` newtype with justified `unsafe impl
    Send/Sync` (the descriptor is allocated once on the server thread,
    every pool thread hands the same pointer to `CreateNamedPipeW`, which
    copies it per instance; nobody mutates or frees it off the owning
    thread), one `Arc` wrap in `serve_loop`, and the three test call
    sites build their own descriptor the same way.
    `cmux-shell/build.rs` clippy fixes (`&Path`, `is_some_and`).
    Release lane green: cmux-shell.exe 10,567,680 B, **cmux.exe**
    1,016,320 B (the CLI's crate-declared bin name — hooks ownership
    rules match `\cmux.exe`, so the speculative installer name
    `cmux-cli.exe` was corrected everywhere), conpty.dll 109,920 B,
    OpenConsole.exe 1,066,296 B, 23.7 s.
  - wxs/scripts repaired against the WiX 3.14 schema: legal Component/
    File IDs (CNDL0014), `Upgrade` inside `Product` (CNDL0005), guarded
    `ProductVersion` define (CNDL1118), un-bracketed `WorkingDirectory`
    (CNDL0275), ARPNOMODIFY removed (WixUI_Minimal already defines it —
    LGHT0091 duplicate), **`-arch x64` added to candle** (ICE80: x86
    components in a 64-bit directory), sign-at-install CustomAction
    removed (signing is a build-time step), machine-wide
    `CMUX_CONFIG_FILE` removed (broke per-user config discovery),
    Manufacturer/ARP links → the GitHub repo, registry key
    `Software\cmux-clone\cmux`, product version 0.0.1, config template
    ships with `Vital=no`, and third-party `NOTICES.txt` (MIT ConPTY
    pair, ghostty/egui stack, portable-pty 0.8.1…) ships in the MSI. The
    port's own LICENSE declaration remains an open owner item — disclosed
    wherever the installer commits it.
  - Result: `port/target/installer/cmux-installer.msi` **5.43 MB** with a
    clean candle+light run (zero errors), and `validate-installer.ps1`
    administrative extraction confirms all six payload files
    (cmux-shell.exe, cmux.exe, conpty.dll, OpenConsole.exe,
    cmux.json.template, NOTICES.txt). A live install/uninstall smoke run
    is owner-manual for now (no elevation/passphrase-free flow inside
    this session); it stays unclaimed until run.
  - Gates re-run green after the code changes: `cargo fmt`, `cargo clippy
    --all-targets -- -D warnings`, workspace `cargo test` (every crate —
    CLI 15, config 17, hooks 37, ipc 51 incl. the pooled-instance suite,
    layout 27, pty-port lanes, shell 120+89, toast 8, webview2 7,
    workspaces 11 — all pass), `cargo test -p egui_tty` (62 + 3 doctests),
    `cargo build`.
  - Next: GitHub Release publication (no `gh` CLI/PAT on the box — owner
    uploads via web UI or supplies credentials), tag-driven release CI,
    silent install/uninstall smoke, signing, and the port's own LICENSE
    decision.

- **2026-10-27 - round 9 (sidebar depth: overflow scroll + drag-reorder
  + inline rename)** - the workspace strip's three interaction stacks
  landed as one increment (ADR-0010 amendment below;
  figma-design-reference.md sec. 22).
  - Wheel scroll: the strip became ONE scroll column (workspace rows +
    services segment; upstream's LazyVStack rule), wheel over the
    content region only (the terminal lane's hit region excludes the
    aside), edge fades both ends (solid-fill alpha mesh, 16pt),
    reveal-on-select (scroll_to_reveal, min motion), drag autoscroll
    (24pt margin, 14pt/frame ramp). Pure math in
    `cmux-shell/src/sidebar.rs` (`scroll_clamp`, `visible_row_range`,
    `scroll_to_reveal`, `insertion_slot`, `reorder_target`,
    `indicator_top`, `drag_autoscroll`, `rename_commit`,
    `preferred_name`) with 10 unit tests.
  - Drag-reorder: `SidebarDrag` session opens on the source row's
    first decisive-drag frame (>6pt move / >0.8s hold / leaving the
    row), commits `Action::ReorderWorkspace{from,to}` gated by the
    planner verdict (no-op drops commit nothing, upstream
    `suppressesNoOp`), the active selection follows BY ID, a release
    frame never selects, and the 2pt accent insertion line paints
    inset 8 while the drag holds. Click suppression:
    `row.clicked() && !dragging_this`.
  - Inline rename: double-click opens the buffered editor seeded with
    the row's display name (`PaneBody` TextEdit without a frame),
    commit = Enter / focus loss through `rename_commit` (trim; empty
    never writes; retyping the auto-name freezes nothing), Escape
    cancels in ONE press (upstream parks the caret twice - divergence
    recorded), and the editor auto-cancels when its row slides out of
    the scroll window (`content_clip.contains_rect`).
  - Probe: `port/harness/test_sidebar_depth_r9.py` **35/35** on the
    final binary (two legs: seeded 12 → pin-baseline wheel round
    trip + terminal no-steal, exact planner click-drag, scrolled
    drag by id, rename commit + Escape cancel, WM_CLOSE → session
    round trip incl. `customTitle` + saved order + restored active).
  - Registry note: the reorder/rename actions existed unbound with
    `None` strokes in DEFAULT_BINDINGS; they stay keyboard-unbound
    (no upstream strokes for them) and now ride the sidebar gestures.
  - Divergences recorded: one-press Escape cancel; no floating drag
    image (dimmed source row + 2pt accent line instead); fade =
    solid-fill alpha ramp over a Mesh (upstream: black scrim with
    stops 0.05/0.25/0.65/1.0). Deferred still: per-workspace colors,
    WAIT/RUN/IDLE/ERR state rollup, metadata rows (~sec. 12 rows).
  - Probe-side lessons (recorded in the battery): strip scroll states
    are pinned via IPC select (the app's reveal math) instead of
    pixel sensing (chip #1d1f26 vs sidebar bg #191b22 differ by 4/255
    - structurally unreliable); SendInput wheel maps positive
    mouseData → wheel-UP (egui dy positive → scroll decreases); a
    hovered inactive row paints a soft hairline fill, so gesture
    probes park the cursor off the strip before pixel comparisons;
    `PrintWindow` needs `PW_CLIENTONLY|PW_RENDERFULLCONTENT` or the
    caption shifts every client pixel.
  - Gates on the final binary: fmt 0, clippy `-D warnings` 0,
    workspace `cargo test` **386/0** (24 suites; r9 adds the sidebar
    module's 10 + main.rs's 4), build 0. Regression ladder green:
    lane-4 24/24, m43 19/19, m51 99/99, restore 51/51. (The round-8
    docs referenced `round6_ws_probe.py`; that file is absent from
    the workspace checkout - likely owner-disposed - so the r9
    battery is the strip probe of record.)

- **2026-10-27 - round 8 (owner: "in the status bar remove
  1:powershell* and keep going"; "for my decision just do the
  recommend"): the pane-marker run retired + the sec-20 residual
  executed** (figma-design-reference.md sec. 21).
  - Marker removal: the strip now paints ws identity | telemetry |
    right cluster; `status_marker_label` + its cap + two pins left,
    the fold matrix lost `marker_needs`, and the a11y label still
    counts real panes. Round-6 ws probe re-authored to
    marker-retirement rules (strict info-blue shape test - the old
    tol-90 box matched the identity's secondary ink); 15/15.
  - Recommended decisions executed: NEW browser leaves land on a
    token-painted data-URL home (`browser_home_url(viewport_backgro
    und)`, title-less/content-less, theme-rederiving; restored
    leaves keep saved URLs), closing the `about:blank` gray for new
    panes; terminal cells and the chrome typeface recorded keeps.
  - Gates: fmt 0, clippy 0, tests 372/0 UNPIPED (24 suites; -2/+1
    pins), build 0; m51 `99 passed, 0 failed` on the rebuilt binary.
  - Live: `cmux_live_11.png` on-token 97.02%; strip info-blue 0 px;
    drift vs `cmux_live_10.png` (2305 px) entirely inside the strip
    band; owner instance relaunched.
  - Executed under the owner's standing decision rule, recorded
    post-round in ADR-0016: delegated owner-gated options run the
    recommended choice + disclosed, not parked.
- **2026-10-26 - round 7 (owner: "no i mean the color theme for the
  whole application dosen't match the color theme of the figma"):
  whole-window palette audit vs sec 2 + the browser default
  background token** (figma-design-reference.md sec. 20).
  - Owner config ruled out first (`cmux.json` has no `theme` block);
    full-window census of `cmux_live_8.png`: the four sec-2 base
    tokens cover ~97% (`#0c0e14` 66.02%, `#191b22` 27.24%,
    `#1d1f26` 1.91%, `#11131a` 1.64%); off-token pixels are
    hairline/edge/glyph AA only. Terminal zone 99.27% token fill
    with Ghostty-default cell text (design does not spec cells -
    owner-gated candidate, not changed). Palette overlay re-shot via
    a command-bar CLICK (SendInput Ctrl+K unreliable at this DPI -
    the control diff was 96 px of status-bar digits).
  - The fresh browser leaf's flat `#121212` root-caused to Chromium's
    theme-derived base paint (green-page proof: Chromium content
    reaches PrintWindow captures, so the app paints nothing there).
    WebView2 now threads the design viewport token:
    `BrowserSpec.default_background` (0xRRGGBB) at the single spec
    call site, controller requested through
    `ICoreWebView2Environment10` options + `ControllerOptions3::
    SetDefaultBackgroundColor` (documented early-init path for the
    dark-mode base paint; upstream issues 4389/5752), plain-call
    fallback for older runtimes, wire-time `Controller2` put as
    backstop, `log_once` diagnosability, opaque `corewebview2_color`
    packer (+ unit pin).
  - Canvas ladder verified live on Evergreen 156.0.4314.42: plain
    no-CSS pages paint `#0c0e14` (297,721 px; computed html/body
    `rgba(0,0,0,0)`), explicit token pages paint over it, and the
    bare `about:blank` leaf keeps Chromium's theme gray `#121212`
    byte-identical through the fix (Edge special-cases the blank
    document - M5.1's default URL). Residual owner-gated: keep
    upstream `about:blank`, or land a token-painted default data URL
    (product-visible; either choice awaited).
  - Gates: fmt 0, clippy `-D warnings` 0 (exit-code verified),
    workspace tests 373/0 UNPIPED (24 suites; +1 pack pin), build 0;
    m51 battery re-run on the rebuilt binary `99 passed, 0 failed`,
    exit 0. Figma MCP re-attempted once: create_session OK,
    screenshot/metadata still Starter-plan paywalled (recorded in
    sec 20).
  - Live: `cmux_live_9.png` reproduces `cmux_live_8.png`'s token
    shares exactly (66.02/27.24/1.91/1.64).
- **2026-10-26 - round 6 (owner: "okay remove it" + "this 1:powershell
  in blue does not change when I change workspaces - remove it or match
  it with the current running workspace"): the cmux-win11 chip left the
  status strip, and the strip leads with the active workspace's
  identity segment** (figma-design-reference.md sec. 19).
  - Chip removal reverses the sec-16 keep-back: the paint block left
    `draw_status_bar` and the UTF-8 + `pipe:` leaf segments re-anchored
    to the strip's right pad (`bar.right() - 8`).
  - "Match" over "remove": the marker run already read the active
    workspace by construction (markers build from
    `workspaces.active`'s tree), but same-shell pane titles folded the
    live name to `powershell` in every workspace - so the segment leads
    with the minted `w{N}` id + the live name (`w1 · powershell`,
    secondary ink, muted separator before the marker run); the a11y
    label rides the painted label.
  - Fold matrix (`status_ws_fit_case`): the pipe-leaf tail (14 → 8
    codepoints) flexes first, then the ws name; the marker run and the
    minted id never fold; `free_leafless` subtracts both 12px
    right-cluster anchor gaps so the helper and the paint share one
    inequality.
  - Round-6 bug found, diagnosed, fixed: `markers_max_x` still
    subtracted `ws_span` while the fit cursor already starts past the
    painted segment - the span counted twice and one-pane marker runs
    fell behind a bare ellipsis while the full label painted (live8
    repro; temporary `strip-fit` eprintln diagnosed frame values, then
    left the binary). Fix keeps
    `markers_max_x = right_clamp - 10 - telemetry_span`.
  - Gates: fmt 0, clippy `-D warnings` 0, workspace tests 372/0 UNPIPED
    (round-5 total 369 + 3 new pins), build on the cleanly closed
    app; release lane still stops at `libghostty-vt-sys` build.rs
    (zig toolchain absent - pre-existing, unrelated).
  - Probes: `round6_ws_probe.py` 14/14 (fixture `w2` mint + activate,
    captures A/B, identity zone 61 px diff on switch); battery
    first-run green: m43 19/19, a11y-uia 3 pass / 4 standing skips /
    0 fail, titlebar-2b 13/13 + 13/13, lane-3 15/15 + 22/22, lane-4
    24/24, lane-5 36/36, m51 99/99.
  - Live: `cmux_live_8.png`; strip reads
    `w1 · powershell | 1:powershell* | CPU: 26% MEM: 10 GB CLAUDE:
    IDLE … pipe: …09267108-1001 UTF-8`; the strict-info blue run
    restores at x 465..554 (186 px = live7's count shifted right by
    the ws span); scanner `sidebyside_r6.py` 24/0 and the drift
    live7→live8 = 6524 px, ALL in the status bar (aside/title/
    terminal zero).
- **2026-10-26 - round 5 (owner: "finish all the design rounds"): the
  parked Figma side-by-side is closed as a numeric reconciliation**
  (figma-design-reference.md sec. 18): Figma MCP file reads stayed
  paywalled on the Starter/View-seat plan (session opened,
  `get_metadata` answers the rate-limit banner), so the comparison ran
  against sec 1-17's verified node reads with one fresh capture
  (`cmux_live_7.png`, plain relaunch, restored session): 23 checks /
  23 pass, 0 fail; the owner divergences classified EXPECTED (band
  absent / version strip absent / `cmux-win11` chip kept, the latter
  still an open owner question). Drift vs `cmux_live_6.png` is
  session state only - 0 diff px in the title strip and the terminal
  zone. Ring lesson recorded: at this DPI the ring's horizontal edges
  paint subpixel-AA cyan-family pixels (top edge y=41, 797 family px);
  the battery's family predicate is the correct rule, exact-only scans
  false-negative. No code changed; the residual ledger is reconciled
  in sec. 18's table.
- **2026-10-26 - round 4 (owner: "the bar that tells the instruction
  that has the short cuts — remove it too"): the design lane 3
  sub-header band is entirely gone** (figma-design-reference.md
  sec. 17; sec. 11 is the restoration spec).
  - Layout: `subheader_rect` + `SUB_HEADER_HEIGHT` removed; `main_rect`
    starts at the title row's bottom (32pt) — panes and webview
    children sit directly under the title strip; the palette overlay's
    anchor dropped to `title+8` (40pt).
  - Dead private code removed with the surface: git sampler fields +
    `refresh_git_sample` + `GIT_SAMPLE_INTERVAL_MS` + the
    `cmux-git-watcher` spawn block (`cmux_shell::git_state` parsing
    lib stays public with its tests), `humanize_key`/`keycap_tokens`
    + their test, `paint_vsplit_glyph`/`paint_equalize_glyph`,
    `TOKEN_AGENT_INK`/`agent_chip_bg`. Restoration bodies for the
    small items live in the sec. 17 appendix.
  - Affordances (disclosed): tray buttons + keycap pills were the
    band's only click surfaces; splitting stays on the owner keymap
    (`ctrl+shift+r`/`d`, verified), `equalizeSplits` remains
    config-bindable (unbound default, unchanged) but surface-less;
    the palette has no builtin equalize row. Git/diff/agent chip data
    shows nowhere in the UI now.
  - Gates: fmt 0, clippy `-D warnings` 0, tests 369/0 (one fewer unit
    pin), build on the cleanly closed app. Pixel-verified
    (`cmux_live_6.png` vs `_5.png`): exact band-fill 12444 px -> 0,
    ring top-edge 0 -> 740 px, chrome cluster zones identical.
  - Probes: lane-3 rewritten as the absence probe (15 visual + 22
    behavior); its bar-click failures traced to its own unverified
    foreground pin (WindowsTerminal overlap; lesson applied), and
    lane-5 re-anchored for the risen-by-32pt panes (`90*s`->`58*s`,
    `66..84*s`->`34..52*s`). Battery green first-run except lane-3's
    expected pin work: titlebar-2b 13/13 + 13/13, lane-3 15/15 +
    22/22, lane-4 24/24, m43 19/19, lane-5 36/36, m51 99/99.
- **2026-10-26 - round 3 (owner: status-bar CPU/MEM font smaller;
  remove the version display): 10px telemetry run, aside version
  strip dropped (figma-design-reference.md sec. 16; parity row 23).**
  - The `|` + CPU/MEM + agent-state telemetry run paints 10px now
    (pane markers keep 12px); the reserve measures with the same
    font the paint uses, so the round-1 no-collision construction
    holds with a narrower reserve (overflow markers still fold
    first). Live metrics: the identical `CLAUDE: IDLE` block 11px →
    9px tall; the telemetry run's right edge 43px earlier; marker
    and right-cluster zones unchanged.
  - The aside bottom strip (design node 1:485: chip fill + top
    hairline + the version chip with the info-blue dot) is gone BY
    OWNER DECISION — it was the literal version display
    (`v` + CARGO_PKG_VERSION); its 38pt layout budget released to
    rows (bounds now 6pt above the aside bottom, matching the rows'
    own inset); `SIDEBAR_BOTTOM_STRIP` + `status_version_label` +
    the unit pin removed. The status bar's `cmux-win11` product
    chip shows no version and stays; the ask's ambiguity (the owner
    might have meant that chip) is disclosed in the report.
  - Gates: fmt 0, clippy `-D warnings` 0, workspace tests 370/0,
    build on the cleanly closed app (WM_CLOSE only). Pixel-verified
    live (`cmux_live_5.png` vs `_4.png`): aside-bottom band `#1d1f26`
    + 180 ink px → plain `#191b22` + 0 ink. Battery on the round-3
    binary, all first-run green (zero flakes): titlebar-2b 13/13 +
    13/13, lane-3 17/17, lane-4 24/24, m43 19/19, lane-5 36/36,
    m51 99/99. No probe sampled the removed band or the telemetry
    glyphs (grep audit); the live session restored the owner's
    three-workspace snapshot on relaunch.
- **2026-10-26 - design-fidelity round 2 (owner: "do as figma",
  weird close symbol, missing + button): sub-header edge fix,
  hand-drawn x glyph, theme block dropped
  (figma-design-reference.md sec. 15; parity rows 1/23).**
  - Root cause 1 (verified by pixel scan of `cmux_live_3.png` + code
    read): the sub-header strip painted from the WINDOW edge while its
    own design comment says "main area only" — its `#191b22` fill rode
    over the aside's top band, covering the `+` (new workspace) row
    the code paints beneath it (verified code read: the + glyph paints
    at pt (128,53); the pre-fix capture shows zero ink in the + band,
    i.e., covered — the post-fix capture shows the glyph and the
    click-probe proves the widget). `draw_sidebar` is now
    computed BEFORE `subheader_rect` and the strip starts at
    `sidebar_rect.right()` (the aside keeps its full height under
    the title row per node 1:399).
  - Root cause 2 (verified by python byte audit): the close glyph's
    source literal carried mojibake bytes `C3 83 E2 80 94` (a
    double-encoded "Ã" + em-dash) — the app painted "Ã—"; earlier
    correct-`×` grep reads were cp1252 console artifacts and byte
    audits are ground truth. The glyph is now two hand-drawn
    `painter.line_segment` strokes (arm 4.5, muted/primary-by-hover
    ink) — font- and encoding-independent.
  - Owner authorized dropping the legacy theme block from their
    `cmux.json` ("do as figma"): file otherwise byte-identical;
    the config-wins ladder itself unchanged (unconfigured = exact
    Figma tokens).
  - Comment-layer mojibake elsewhere (about 90 sites, valid UTF-8,
    cosmetic, comments only) recorded for a later normalization
    pass — out of scope.
  - Tests: no new units (workspace `cargo test` 370/0); fmt +
    clippy `-D warnings` clean; build on the closed-app path.
    Lane-3 left-chip probe scans re-anchored to the sidebar edge
    (`round(256·s)`, 0.72/0.80·cwidth windows) after the strip
    moved right.
  - Battery on the round-2 binary, first-run green (zero flakes):
    titlebar-2b visual 13/13 + behavior 13/13, lane-3 17/17,
    lane-4 24/24 twice, m43 19/19, lane-5 36/36, m51 99/99.
  - NEW behavioral proof (`plus_click_probe.py`, temp, owner
    disposal): the + click creates a workspace — IPC
    `workspace.list` before=1 after=2 — using the 2b pin recipe
    with a VERIFIED foreground-pin retry loop, `ClientToScreen`
    mapping and a `WindowFromPoint` app-owner guard, followed by a
    graceful WM_CLOSE teardown. The earlier probe no-ops
    (SendInput and PostMessageW) were the probe's UNVERIFIED pin
    (the recorded overlay-eats-clicks profile), not the app —
    same-binary 2b behavior 13/13 shows click delivery is sound.
  - Live verification (owner env, `cmux_live_3.png` before /
    `cmux_live_4.png` after under `%TEMP%\opencode`): palette exact
    — sidebar `#191b22`, chips `#1d1f26`, title strip + command
    bar `#11131a`, pane interior + status-bar fill `#0c0e14`, ring
    `#38bdf8` at x=320px, sub-header fill `#191b22` (design read
    at figma-design-reference.md line 32); + ink 18 px at pt
    (128,53); close strokes 24 px (y 104..115). Operator-slip
    disclosure: one hard-kill of the app instance happened by
    mistake mid-round (a `Stop-Process` executed before a chain
    error aborted the command); the clean-exit rule was re-applied
    and the next relaunch restored the pane from the last clean
    snapshot, re-exercising the row-22 repair path.
- **2026-10-26 - design-fidelity round 1 (owner report "not identical -
  the color, the responsive"): chrome/theme coherence, design sidebar
  width, friendly title fold, status-bar budgets
  (figma-design-reference.md sec. 14; parity row 23).**
  - Root cause (live captures + the owner's `cmux.json` read): their
    file carries the legacy "Apple System Colors" theme block
    (`background #1e1e1e`) and the theme precedence made only SOME
    surfaces follow it — title strip, sidebar, gutters, pane
    interior — while the sub-header band, status-bar fill, product
    chip, command bar and every chrome chip/pill/row painted raw
    design tokens: chrome split between two schemes (live: strip
    `#1a1a1a` under sub-header `#191b22`, chips `#1d1f26`, interior
    `#1e1e1e`). The same capture showed the aside 216pt wide against
    design node `1:399`'s 256, and Windows PowerShell 5.1 titling its
    console with the absolute image path
    (`C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe`),
    which overflowed the breadcrumb chip, the sidebar row and the
    status-bar marker (the last collided with the right cluster).
  - Theme ladder (ADR-0008 divergence 7 completed): EVERY chrome fill
    paints through `ThemeRuntime` — `background`/`sidebar_background`/
    `viewport_background` plus a NEW `chip_background` (default the
    `TOKEN_CHIP_BG #1d1f26` design token; derived
    `shade_lighter(sidebar)` when `theme.background` is configured —
    the inverse of `shade_darker`, channels rounded + clamped; the
    design ladder `#191b22 / 0.88` rounds to `#1c1f27`, within a
    channel of the flat chip token: recorded approximation). 19
    raw-token sites routed (5 viewport, 3 main, 2 sidebar, 9 chip).
    Pane-INTERIOR faces (agent-lane cards `#191b22`/`#1d1f26`/
    `#0c0e14`, tab `#282a31`) stay RAW design tokens — content, not
    chrome; the lane-5 spec pins them.
  - `SIDEBAR_WIDTH` 216 → 256 (the design width). Probe geometry
    tables re-anchored to live-checked 256 geometry, never guessed
    fractions: lane-4 `fill_x1=int(254·s)` + badge zone
    `(256-46..256-6)·s`; lane-5 agent leaf
    `x0 = ((cwidth_pt + 256 + 12) / 2)·s` (balanced 2-leaf split,
    12pt gutter); m43 `SIDE = 256`; m51 `sidebar_px = round(256·s)`.
  - Friendly OSC-title fold: `friendly_osc_title` folds an absolute
    drive/UNC path whose LAST segment ends in `.exe`
    (case-insensitive; empty stem not folded) to the image stem,
    applied in `display_title`'s osc fallback; `title_override`
    never folds (the M6 ssh identity rule stays). The same fold
    Windows Terminal ships for the same OS behavior.
  - Status-bar budgets: `draw_status_bar` measures the right cluster
    FIRST (it owns the fixed right edge), reserves the CPU/MEM/agent
    telemetry before laying out any marker, caps marker titles at
    `STATUS_MARKER_MAX_TITLE_CHARS = 14` codepoints (head plus the
    fold ellipsis), and overflow markers drop behind a muted
    ellipsis riding the marker/separator gap. The IPC leaf folds at
    `STATUS_IPC_LEAF_MAX = 14` (leading ellipsis plus the last 13) —
    the owner's SID-derived default pipe (`cmux-s-1-5-21-…`,
    ~50 chars) had overflowed its segment exactly like the titles.
  - Tests: +4 units (2 friendly-title pins, marker-cap,
    `shade_lighter`) plus theme-test extensions in place; workspace
    `cargo test` 370 (was 366); fmt + clippy `-D warnings` clean;
    `cargo build` finished (the IPC-fold rebuild needed the running
    app closed first — the exe locks while it runs; clean exit via
    WM_CLOSE, never a taskkill).
  - Battery on the final binary: titlebar-2b visual 13/13 + behavior
    13/13 (behavior flaked 12/1 and 8/5 earlier; each cleared on the
    first rerun — the recorded launch-timing profile), lane-3 17/17,
    lane-4 24/24 twice after re-anchoring, m43 19/19 (cleared after
    one 15/4 flake), m51 99/99, lane-5 36/36 with the new band
    formula.
  - Live verification with the owner config in effect (plain relaunch,
    no env overrides; captures `cmux_live_1/2/3.png` + zoom crops
    `cz*.png` under `%TEMP%\opencode`): strip/sub-header/sidebar/
    gutters all `#1a1a1a`, interior/status bar/chips all `#1e1e1e` —
    one coherent ladder; ring `#38bdf8` at x=320px (the 256pt edge);
    breadcrumb/row/marker read the folded `powershell`; status bar
    `1:powershell* | CPU: 30% MEM: 11 GB CLAUDE: IDLE` + folded
    `pipe: …09267108-1001` + `UTF-8` + `cmux-win11`, zero collisions.
    Open owner decision: dropping the legacy theme block from their
    `cmux.json` (their file — untouched) would put the producer on
    the exact design palette; the config-wins contract itself stays,
    never the reverse.
- **2026-10-26 - design-fidelity ladder lane 5 closed: agent activity
  stream pane (figma-design-reference.md sec. 13; parity rows 8/9/10
  behavior + row 23 chrome-lane record).**
  - Design nodes 1:244 (pane) / 1:257 (stream) with completed card
    1:258 and active card 1:299. New `PaneBody::Agent(AgentActivityPane)`:
    no child process (closes only by user action or with its
    workspace), a click interceptor that models focus like browser
    panes, a11y label "Agent activity", read-only posture
    (`surface.send_text`/`surface.read_text` refuse it via the
    has-no-terminal path), wheel-scroll pans a bottom-anchored viewport
    (offset clamped against measured content before painting).
  - Painter per the pinned spec: tab row `#282a31` 20pt + hand-drawn
    agent glyph + "Agent activity" + the pending warm chip
    (`#605022` fill / `#ffc42f` uppercase agent label); stream fill
    `#0c0e14` inset 8; completed card `#191b22` (hand-drawn
    circle-check, headline `#e2e2eb`, real elapsed time `#87929a`,
    detail `#bdc8d1`); active card `#1d1f26` + 1px `#ffc42f` ring
    (accent dot + uppercase label + RUNNING + the frozen terminal tail
    on a `#0c0e14` block + the real pending age at the footer). A
    measure pass sizes each card from laid-out text.
  - New pure `cmux-shell::agent_cards` module (9 unit tests: headline
    rule incl. the OSC-9 first-line lift, eviction at
    `CARDS_MAX_PER_WORKSPACE = 24`, `sync_len` slot semantics,
    stale-index no-op, per-workspace clear). REAL ingest sites only:
    `PaneEvent::Notifications` (OSC 9/777), the turn-complete delivery
    (agent display name + "Task completed" verbatim), and every
    validated hook-pipe event with journal routes included
    (`feed`/`pre-tool-use` land as cards only - they still never ring
    or toast). `agent_stream_snapshot()` walks the painting
    workspace's card slot plus each terminal pane's pending boundaries
    (arrival = `deadline - delay`; tail frozen at see time).
  - Wire: `agent.open_split` joined `APP_METHODS` (49 to 50 members),
    closing a parsed-but-unserved protocol gap (the CLI already
    carried the reply shape); CLI verb `cmux agent [--surface <spec>]
    open [--json]` (port extension - upstream has no agent verb; other
    subcommands refuse with the docs pointer). Reply:
    surface_id/pane_id/workspace_id + `placement: "split"` +
    `"type": "agent"`.
  - Truthful labels where they had drifted: `surface_rows` reports
    `PaneBody::Agent` as "agent" (it was labeled "terminal"), matching
    identify's type arm and the restore predicate.
  - Restore truthfulness: `workspace_from_outline`/`layout_from_outline`
    gained a `kind_of` closure so agent leaves save
    `panel_type: "agent"` (no browser URL, like browser leaves saved
    theirs in M5.1); `build_tree_from_layout` now takes the snapshot's
    `panels` slice and rebuilds an agent leaf via `new_agent_pane` - no
    shell spawn, empty stream (cards are not persisted). Regression
    test `workspace_snapshot_records_an_agent_leaf_panel_type`.
  - Probe: port/harness/test_agent_activity_lane5.py 36/36 twice on
    the clean rebuilt binary (open + reply shape, `agent` labels,
    tab/stream fills, empty baseline, hook growth incl. the journal
    route, the m34 boundary with the accent-ring cycle, close
    semantics, plain-mode grammar, relaunch round trip). Probe lessons
    folded into reference sec. 13: live-measured band geometry
    (instrumented paint print, since removed), one-invocation m34
    hand-over, `n3 > n2 + 2` sampling noise, stdin `CMUX_SURFACE_ID`
    hook seeding (pixels, not CLI output - the CLI fails open), and
    `pin_foreground` after background restores - the titlebar-2b
    behavior probe gained that helper (discovery + after
    SW_RESTORE-from-minimize) and re-passed 13/13 with NO app change
    (the earlier close-button "failure" was probe-side; the app's
    close path verified clean in a standalone diagnostic).
  - Verification: fmt + clippy `-D warnings` clean; workspace
    `cargo test` 366 (was 335) including the +9 agent_cards suite and
    the agent-snapshot regression; `cargo build` finished; full
    battery on the rebuilt binary: smoke-cli 49/49, m34 turn PASS, M4.1
    16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, restore 51/51, SSH 28/28,
    m51 99/99, a11y 3 pass / 4 standing accesskit skips / 0 fail,
    titlebar-2b visual 13/13 + behavior 13/13, lane-3 17/17, lane-4
    24/24, lane-5 36/36.
  - Open: prompt-footer lane 5b + the diff-preview/gate cards stay
    design residuals (no data plane); MSI/signing assets remain
    owner-provided.

- **2026-10-24 - design-fidelity ladder lane 4 closed: aside RUNTIME
  SERVICES segment (feature-parity row 12); figma-design-reference.md
  sec. 12.**
  - New `cmux-shell::port_state` (pure core 10 unit tests: collect_rows
    shape, STOP latch keyed `(workspace_idx, root pid, port)`,
    `MAX_SERVICE_ROWS` cap, cycle-safe BFS `member_set`, and a REAL-OS
    integration binding a socket -> UP then dropping it -> STOP) +
    Windows snapshot plane (`cfg(windows)`: GetTcpTable2/GetTcp6Table2
    over windows-rs 0.62.2 IpHelper/WinSock/ToolHelp/Foundation).
    Trap found and fixed: windows-rs types the table as `[ROW; 1]`, so
    `table.table.get(..N)` silently returned nothing - indexed through
    `std::ptr::addr_of!(table.table)` + `from_raw_parts(first,
    dwNumEntries)` for both v4/v6; ports decode
    `u16::from_be((dwLocalPort & 0xFFFF) as u16)`; AF_INET/AF_INET6 are
    `ADDRESS_FAMILY` wrappers (use `.0`).
  - Data plane: 2s `cmux-service-watcher` thread samples ALL
    workspaces' pane roots (tree pid captured at shell handoff:
    `start_shell` now returns `ShellHandoff { stream, events, status,
    tree_pid }` stored as `Pane.tree_pid` at both construction sites);
    `refresh_service_sample()` feeds the sidebar painter block after
    the workspace rows (header always drawn; rows `#0c0e14` r4 p4,
    warm `:port` `#ffdf9f` + UP badge `#8ed5ff`, muted STOP badge;
    `TOKEN_WARM_INK` added) with an a11y labeled region; failed
    snapshot ticks send nothing and log failure/recovery once.
  - REAL bug fixed on the way (M6 semantics, row 14): upstream's
    `CmuxConfigExecutor` (196) types `command + "\n"`, byte-faithful,
    but a raw LF is NOT Enter for pwsh's PSReadLine - commands sat on a
    `>>` continuation prompt (observed live) and never executed, and
    the smoke's marker check had been passing on the visible but
    unexecuted text. The port now types `command_input_bytes(text)` =
    `text + "\r"` (CR is the Windows Enter; submits in pwsh and cmd)
    at both sites (IPC `command.run` + palette activation); regression
    test `executed_commands_end_in_cr_not_lf`; lane-3 palette probe
    re-run green (17/17) on the rebuilt binary.
  - Probe evidence: `test_sidebar_services_lane4.py` 24/24 three
    times. Two defects between the first run (10/3) and green, both
    root-caused: (a) the probe's geometry table assumed the sidebar
    rect starts at client y=0 - a live per-line ink profile over the
    sidebar column placed the real bands +32pt lower (sidebar rect
    starts below the strip; workspace text mid 87pt, header
    116.8..125.6pt, warm numerals 140.8..147.2pt) and the table was
    corrected to the verified geometry; (b) the pane-exit phase typed
    `exit` + a raw LF, which again landed on the `>>` continuation (the
    probe now sends CR; the phase asserts the app closes cleanly after
    the last pane exit, also proving the sampler survives the vanished
    root without hanging the frame loop).
  - Battery on the rebuilt binary (both cmux.exe and cmux-shell.exe
    changed this lane): smoke-cli 49/49, smoke-m34-turn PASS, M4.1
    16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, restore 51/51, SSH 28/28,
    a11y 3 pass/4 standing skips/0 fail, m51 99/99, titlebar-2b visual
    13/13 + behavior 13/13, lane-3 probe 17/17, lane-4 probe 24/24;
    workspace `cargo test` 335 (was 324: +10 port_state, +1 CR
    regression), fmt + clippy `-D warnings` clean, `cargo build`
    finished.
  - Flake/probe ledger additions: (a) titlebar-2b probe obsolescence
    fixed - the lane-3 command bar occupies the strip's middle (anchored
    `max(0.3326*w, 335)pt`), so the blank-strip sample and the caption
    dbl-clicks moved from 0.62W to a fixed 300pt anchor (past the
    worst-case chip end 277pt; blank at any width, also where the bar
    hides <800pt); first behavior run after it showed left-edge resize
    growing 34px for a 60px drag (below the 40px bar) - cleared on
    rerun with 13/13, same variance family as the m43 drag anchor; (b)
    lane-4 probe phase-4 newline gotcha recorded (raw LF != Enter; CR
    everywhere the probe wants execution); (c) m51's historical
    34-pass timeout abort did not reproduce this run (99/99 first try).

- **2026-10-24 - design-fidelity ladder lane 3 closed: sub-header chips
  (row 11) + command bar / palette (row 13); figma-design-reference.md
  sec. 11.**
  - Implementation per the extracted sub-header spec (nodes 1:5, 1:7,
    1:14, 1:21, 1:29, 1:54): new 32pt band between the strip and the
    panes (`SUB_HEADER_HEIGHT`), fill #191b22, chips `top+5` h22 r4:
    git chip (prompt glyph + abbreviated cwd + `[branch*]` badge),
    staged-diff chip (Bold `+n`/-n` + `staged`), violet agent chip
    gated by the real M3.4 turn-boundary predicate; right cluster of
    two effective-stroke keycap pills (v-split/h-split labels from
    DEFAULT_BINDINGS; unbound hides) + a `#1d1f26` tray holding
    `#11131a` buttons wired to `SplitFocused(Right)` (first live path
    for the action from chrome) and `EqualizeAll` (previously
    unreachable). Design's tray 3 cross + Reset pill stay recorded
    residual-skips (no port features); no-repo / no-staged variants are
    hidden, not invented.
  - Git data: NEW `cmux_shell::git_state` sampler (rev-parse +
    status --porcelain -b + diff --cached --numstat) over the process
    spawn cwd, 2s cadence on its own waking channel; pure parsers
    (`parse_status_porcelain`, `parse_numstat`, `abbreviate_home`)
    unit-tested 10/10. Documented limitation: no OSC 7 pipeline yet, so
    a `cd` inside a pane does not move the chip.
  - Command bar (node 1:512, row 1): fill/ink/pills per the mock; the
    bar derives purely from the client width
    (`title_command_bar_rect_pts`: left max(0.3326W, 335pt), width
    min(420, W-148-left), hidden below 800pt) and the wndproc carve
    (`strip_metrics` 4-tuple) keeps hit + resize-band decisions and
    the painter agreeing by construction (`title_bar_hit_decision` now
    8-arg, unit-tested with the carve).
  - Palette (row 13, no design node - recorded extension): #11131a
    420pt modal anchored 8pt below the chrome, query line + wrap
    arrows + Enter rows (config commands first, workspace-flavor
    skipped; then cross-workspace jump rows); runs before
    `process_bindings` and consumes every input event while open;
    command rows execute the exact M6 `command.run` semantics;
    jump rows focus via `pending_jump`. `Ctrl+K` is the new 20th
    DEFAULT_BINDINGS row (`commandPalette`).
  - Fonts: added a separate `Bold` family (`fonts.rs`:
    segoeuib.ttf/arialbd.ttf registered as family "Bold"; Proportional
    order untouched) - 8/8 fonts tests green.
  - Probe: port/harness/test_subheader_lane3.py (battery harness,
    visual 17/17 + behavior 20/20): row/chip/pill/tray/bar fills and
    inks, agent chip absences; bar-click and Ctrl+K open the modal,
    Esc closes, wrap arrows move the selection, Enter runs the
    selected config command into the focused terminal (marker read
    back over `cmux read-screen`), no typing leaks, tray v-split
    splits the pane (gutter verified). Probe findings folded into the
    reference doc: #11131a-vs-#0c0e14 fills need tol<=4; glyph ink
    splits a button's fill run, so click targets take the FIRST
    >=4px run; the app needs an explicit SetForegroundWindow before
    injected input (foreground-activation race was the earlier flake).
  - Test expectation updates from the +32pt chrome: m43's drag anchor
    and m51's pane-top now compute round(64*scale) (19/19 and 99/99).
  - Verification: fmt + clippy -D warnings clean; workspace
    `cargo test` 324 unit tests (git_state 10/10, fonts bold 8/8,
    keymap 20 rows + command-bar rect + palette filter/keycap unit
    tests); full battery on the rebuilt binary: smoke-cli 49x2, m34
    turn PASS, M4 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, m51 99/99,
    SSH 28/28, restore 51/51, a11y 3 pass / 4 standing accesskit
    skips / 0 fail, lane-3 probe 17+20.
  - Flake ledger additions: (a) smoke-cli.ps1's redirected native
    stderr aborts the run at the documented identify/browser-init
    retry sites - the two retry loops now scope
    `$ErrorActionPreference='Continue'` so diagnostics print without
    aborting; the check results still decide (49x2 green); (b) the
    sub-header probe's early failures were probe-side (foreground
    race + tolerance dupes), not app behavior - resolved inside the
    probe (see the probe notes above).
  - Open: lane 4 (aside services segment row 12); lane 5 agent cards
    and MSI/signing assets wait on owner decisions.

- **2026-10-24 - design-fidelity ladder lane 2b closed: custom title
  bar (row 23 extension; figma-design-reference.md sec. 10).**
  - Implementation per the extracted header spec (node 1:495): 32pt
    frameless strip (decorations off), fill #191b22 + bottom hairline,
    left cluster cmux / / / breadcrumb chip (workspace #bdc8d1 <= 140px,
    pane #8ed5ff <= 100px, ellipsize with the shrinking-fit fix), right
    cluster three 44x32 control zones with per-zone hover fills
    (#27292E; close #C42B1C + white ink) and native-scaled glyphs.
  - Window integration (two probe-FALSIFIED beliefs corrected in
    reference doc sec. 10): with WM_NCCALCSIZE=0 the window keeps no
    nonclient area, so "DefWindowProc keeps resize borders alive" does
    NOT hold - the subclass computes the bands itself
    (SM_CXSIZEFRAME+SM_CXPADDEDBORDER via GetSystemMetricsForDpi;
    unit-tested pure `title_band_decision`; top band skipped over the
    control zones and while maximized). A WebView2 child pinned to the
    pane rect eats the band (child HTCLIENT), so `sync_browser_surfaces`
    insets edge-adjacent children by the band width.
    The strip carries an a11y hover region labelled
    "Title bar: cmux / {ws} > {pane}, drag to move window"; the three
    buttons are labeled click widgets.
  - Two production bugs found and fixed inside the lane (both
    reproduced with hung-window diagnostics first): the subclass
    install() stack overflow (re-reading GWLP_WNDPROC poisoned the
    stored original proc into self-recursion) fixed with a capture-once
    guard; the breadcrumb fit loop dropped a character but re-appended
    the ellipsis so the candidate never shrank (endless busy loop) fixed
    with a strictly-shrinking count. No diagnostic gates left behind.
  - Probes: port/harness/test_titlebar_2b.py (battery harness, visual
    13/13 + behavior 13/13): strip fill/corner/hairline/inks/chip and
    the close/max/min glyph pixels; maximize/restore by button,
    caption double-click maximize/restore, RIGHT-edge resize over the
    browser pane AND left-edge over egui surfaces, minimize +
    SW_RESTORE, close exits cleanly. A third top-level window seen on
    healthy runs is winit's 18x18 "Winit Thread Event Target" helper
    (pre-existing, untouched).
  - Test expectation updates from the +32pt client top: m43's
    selection drag now anchors on the first pane row below the strip
    (the old y+12 point slid the window via HTCAPTION and cascaded
    into 5 failures; 19/19 after the fix); m51's pinned-rect check
    expects the pane top at client_y + round(32*scale) (99/99).
  - Verification: fmt + clippy -D warnings + cargo test 327 unit
    tests (new: band decision matrix) - the only code addition; the
    full battery on the rebuilt binary: smoke-cli 49x2 (first run
    5 browser-initializing timing flakes, cleared on retry - same
    family as the m51 ledger), turn PASS, M4 16/16, M4.2 13/13,
    M4.3 19/19, M4.4 8/8, m51 99/99, SSH 28/28, restore 51/51, a11y
    3 pass / 4 standing accesskit-blocked skips / 0 fail.
  - Open: lane 3 (sub-header git chips row 11 + command bar node
    1:512), lane 4 (aside services segment row 12); design-vs-build
    fixed-coordinate sampling stays future work; owner decisions on
    lane 5 agent cards and MSI/signing assets remain.

- **2026-10-23 - design-fidelity ladder lane 2 (status bar half)
  closed: main-area status bar + aside version strip
  (figma-design-reference.md sec. 9).**
  - Status bar: 24px strip across the main area only (the aside keeps
    its full height, so panes end above the bar), fill #0c0e14 (node
    1:371's own class; the sec.2 palette mislabeled the footer
    #191b22, corrected in the doc). Left 12px cluster: tmux-style pane
    markers of the active workspace (`ordinal:title`, focused carries
    `*`, corrected from the `1:zsh!` note), `|`, machine `CPU: n%`
    (GetSystemTimes delta refined over two snapshots every ~0.75s,
    kernel32-only FFI) and `MEM` (GlobalMemoryStatusEx used RAM; the
    GB/MB formatter), `CLAUDE: ACTIVE/IDLE` (inferred semantics: a
    terminal lane in the active workspace holds a pending/delivered
    turn-boundary slot = the M3.4 detector saw an agent session).
    Right 11px cluster: control-pipe leaf as the honest listener
    stand-in (`pipe: <leaf>` / `ipc off`), `UTF-8`, `cmux-win11`
    product chip (token sidebar fill + 0.4 hairline, 4px corners).
  - Aside bottom strip (node 1:485): 38px, #1d1f26 fill, 0.3-alpha
    hairline, workspace rows stop above it, right-hugging version chip
    (#0c0e14 interior, 4px corners, 6px info-blue dot,
    `v{CARGO_PKG_VERSION}` secondary 12px). Settings/help icon links
    stay unimplemented (no destinations yet; residual).
  - A11y: the strip is one hover-sensed region labelled
    "Status bar, N pane(s)"; M6 custom-paint precedent.
  - Verification: fmt + clippy + cargo test (323 unit tests incl. the
    formatter/CPU/label suites), a one-shot DPI-aware PrintWindow
    probe (PASS on the instrumented and shipped binaries, then
    deleted), and the full battery on the shipped binary: smoke-cli
    49x2, turn PASS, M4 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, m51
    99/99 x2, SSH 28/28, restore 51/51, a11y 3 pass / 4 standing
    accesskit-blocked skips / 0 fail.
  - Test expectation updated with the layout change: m51's "webview
    child hwnd pinned to the browser pane rect" compares against the
    client bottom; the pane now ends above the 24pt bar, so the check
    computes `pane_bottom = client_y + client_h - round(24*scale)`.
  - Probe learnings recorded (reference doc sec. 9): un-tampered
    probe threads read logical window sizes under DPI virtualization
    (94.5% of this machine's monitor) — the probe must set
    PER_MONITOR_AWARE_V2 and scale sample points by GetDpiForWindow;
    the bar fill shares the viewport token so "above bar" probes
    cannot distinguish them; near-identical darks need tolerance <=6.
  - Stray-process policy refinement: the resident msedgewebview2.exe
    processes on this box belong to WhatsApp and BatteryWidgetHost —
    never kill the process name; identify cmux-owned children by
    ancestry (they are reaped with their owner anyway).
  - Open: the custom title bar (the second half of ladder step 2)
    stays next — decorations off, drag/snap, native buttons.

- **2026-10-23 - design-fidelity ladder lane 1 closed: exact-token
  chrome pass (row 23 note; figma-design-reference.md sec. 8).**
  - Defaults are the Figma tokens: main bg/gutters #11131a, sidebar
    #191b22 (node-table re-extract; the palette table had omitted the
    aside), terminal pane interior #0c0e14 rounded with the ring
    (browser panes never paint it), chip #1d1f26 for the active
    sidebar row, 1px #38bdf8 focus ring (the 2px default-blue ring is
    gone), gutter-only separation for unfocused panes, aside
    hairlines rgba(62,72,79,0.4/0.3), and design text roles for
    egui-fed inks via install_design_visuals (accent = terminal
    cursor + selection stroke; warning/danger/links = the design
    hues).
  - Config precedence: theme.background pins background, darker
    sidebar, AND pane interior (the chrome-blends-with-cells contract
    the shipped template documents); a light scheme skips the design
    visuals override entirely (no light variant in the file).
  - Verified: fmt + clippy + cargo test (319 unit tests), a one-shot
    PrintWindow pixel probe at exact RGB (sidebar #191b22, active
    chip #1d1f26, viewport #0c0e14, 1px #38bdf8 ring, hairline
    blend), and the full battery on the final binary (smoke-cli 49x2,
    turn PASS, M4 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, m51 99/99
    x2 after three reply-budget flake aborts - root cause unknown,
    cleared on retry, no port change; noted for the flake ledger),
    SSH lane 28/28, a11y UIA lane unchanged (4 accesskit-blocked
    skips stay the recorded state).
  - The owner's ~/.config/cmux/cmux.json carries a personalized
    legacy theme block (Apple System Colors mirror); left untouched -
    config wins over design defaults per the shipped precedence.

- **2026-10-23 - M6 SSH implementation slice closed (parity row 20;
  ADR-0014 amendment 1).**
  - Wire: `workspace.ssh.open` (APP_METHODS 49) parses upstream's
    `runSSHTui` v2 handoff params and re-validates so direct clients
    get the CLI's refusals; the reply mirrors `workspace.create`
    (`workspace_id` + `surface_id`).
  - App: `ssh.exe` panes ride the terminal lane
    (`spawn_workspace_with_spec` create-path refactor); child-exit
    lifecycle and shell-tracked pane identity unchanged; the
    app-known title (destination or `--name`) wins over child OSC
    (Windows ssh.exe self-titles with its image path, live lane
    evidence); `--no-focus` leaves the active workspace alone
    (`push_with_focus`).
  - CLI: `cmux ssh` mirrors upstream `parseSSHCommandOptions` for
    the subset with verbatim refusal texts; flags consume anywhere;
    unknown `--` flags refuse everywhere; mixed-TTY clusters refuse;
    pure `-t`/`-T` runs durably encode as the effective `RequestTTY`
    option (upstream's state machine); `--window`/`--transport`/
    `--session` refuse as not supported by the port; `-A`/`-a`
    consume as pass-through; `--command` + trailing args refuse
    (upstream text).
  - Lane: port/harness/test_ssh_m6.py (28 checks, PASS twice) -
    parser refusals, refused-port connect -> child exit -> pane and
    workspace close with the app alive, silent-listener holds with
    title/children observable, `--name`/`--no-focus`, clean
    WM_CLOSE, ssh children reaped with the app.
  - Binary change -> gates re-ran: fmt + clippy + `cargo test`,
    then smoke-cli 49x2, smoke-m34-turn PASS, M4 16/16, M4.2 13/13,
    M4.3 19/19, M4.4 8/8, m51 99/99 x2.
  - Residuals: managed engine + relay auth flow stay deferred;
    reconnect policy post-ADR; an ssh pane restored from a session
    snapshot returns as a plain configured shell (targets are not
    persisted).

- **2026-10-23 - M6 session-restore verification closed (parity row 22).**
  - New lane port/harness/test_restore_m6.py (51 checks, PASS on two consecutive live runs; no Rust or binary change, so fmt/clippy/test gates and the battery were not re-run).
  - Scenario A round trip (two launches, one store): run1 builds a 3-pane tree (env split-right then split-down) plus a workspace two, every pane marker-echoed live; WM_CLOSE saves; the snapshot keeps version 1, one window, two workspaces, active index 1, the horizontal-then-vertical split shape, terminal panels, and in-range dividers. Run2 restores workspace ids (w1/w2) and pane ids verbatim, lands the active workspace on the saved index, puts focus on the saved focusedPanelId pane, and every restored pane spawns a live shell again. Run2 close: tree structure stable, dividers within 0.05, and the run1 snapshot rotated into session.json-previous (createdAt equality checked).
  - Scenario B resilience: truncated store -> unusable + starting fresh logs, process lives and exits 0; a snapshot with an unknown split orientation (diagonal) workspace beside a valid one -> broken workspace skipped (log), valid workspace restores with wb-good and p93/p94 ids, active index clamps to the survivor; CMUX_SHELL_RESTORE_SMOKE=1 -> restores the crafted tree and the store bytes are hash-identical after exit; broken primary beside a valid -previous -> backup wins (wb-prev + p81/p82 restored).
  - Notes: save skips byte-equal writes and rotates primary to -previous (lib.rs save), so the b3 byte-equality assertion proves restore-smoke never saves (session_paths None), not the skip short-circuit; the restore smoke has IPC disabled by design, so its restore evidence is the startup log line alone.
- **2026-10-23 - M6 accessibility spike closed; ADR-0015 accepted (labels landed, UIA exposure verified-blocked).**
  - Stack facts verified: egui 0.36 ships accesskit 0.24 in-core (non-optional); eframe
    0.36's default-on `accesskit` feature wires accesskit_winit (the port's
    `default-features=false` build had it off); egui_tty 0.2 exposes no accessibility
    API; the port's chrome is custom-painted regions (`allocate_rect`) without labels.
  - Landed: `WidgetInfo` labels on every interactive chrome element ("New workspace",
    workspace rows, "Close workspace <name>", "Terminal", "Embedded browser"); canary
    lane `harness/test_accessibility_uia.py` (window title asserted; childless provider
    tree = SKIP, flips to full assertions when exposure works).
  - Verified blocked: with the feature on, the AccessKit subclass installs and answers
    `WM_GETOBJECT` (nonzero LRESULTs), yet every client (.NET UIA walk, RawView,
    `FromHandle` poke, and real Narrator) sees only the top-level window - including
    for accesskit_windows' own hello_world sample with a full tree and visible window.
    Matches open egui issue #8671 (defect class on 0.36.2). Feature reverted to the
    M5-verified `["default_fonts", "glow"]`; labels kept; no UIA-aware parity claim.
  - Gates clean: fmt, clippy -D warnings, test (307), build. Lane: 3 PASS + 4 SKIP
    (blocked), exit 0. Evidence: docs/research/accessibility-evidence.md (sec. 6).
  - Battery re-run after the binary change: smoke 49 x2, m34 PASS, M4 16/16,
    M4.2 13/13, M4.3 19/19, M4.4 8/8, browser 99/99 x2; the first three browser-lane
    runs hit an unrelated select-after-chord IPC timeout flake (reproduced with and
    without the label change), then green twice in a row on the final binary.
- **2026-10-23 - M6 SSH/remote evidence spike closed; ADR-0014 proposed (row 20 grounded).**
  - Upstream surface verified with file/line references: `cmux ssh` CLI verb
    (ephemeral relay + ssh-config-aware sharing; managed TUI bootstrap vs
    unmanaged interactive fallback, CLI/cmux.swift 13651-13954), session verbs,
    the GPL `cmux-remote` engine (staged POSIX-host bootstrap, every bootstrap
    connection sets `ControlMaster=no`), desktop remote workspaces with
    per-machine trust, and tmux `-CC` mirroring (row 21 stays Blocked).
  - Windows facts verified: OpenSSH client ships with Win10 1809+/Server 2019+
    as an optional feature; ControlMaster/ControlPath unsupported - open
    upstream backlog (Win32-OpenSSH #1328 through 2026, `getsockname failed:
    Not a socket`); `cmux-remote` client lanes POSIX-gated.
  - Proposal recorded: land row 20's Windows subset as a small slice (`ssh.exe`
    panes via a port `cmux ssh` verb, child-exit lifecycle, shell-tracked pane
    identity, never child env); the managed engine is the documented deferred
    scale-up (named-pipe/proxy/connection-sharing ledger in the spike doc).
  - Verification: docs-only spike, no code changed (research/ssh-remote-
    evidence.md, ADR-0014, parity row 20, this log).

- **2026-10-22 - M6 slice 1 landed: custom commands (feature-parity row 14); ADR-0008 Amendment 1.**
  - Upstream contract (verified): `CmuxCommandDefinition` (CmuxConfig.swift 1524-1599) - a required non-blank name, optional `description`/`keywords`/`restart`, and exactly one of `workspace` (a full CmuxWorkspaceDefinition) or `command` (non-blank); the executor (CmuxConfigExecutor.swift 159+) strips invisible-bidi scalars (`sanitizeForDisplay`), authorizes by trust, confirms through a dialog when asked, and types `command + "\n"` into the current terminal.
  - Port slice: `cmux-config` gained `commands: [CommandDefinition]` with upstream's decode-time validation (blank name, blank command, flavor XOR - a broken entry fails the whole read like upstream, so load falls back to defaults) plus the sanitizer as `sanitize_command_text`; `cmux-ipc` grew APP_METHODS 46 to 48 with `command.list` (rows carry name/description/confirm/kind, terminal rows show the sanitized text) and `command.run` (`{name, surface_id?, ack}`, ack bool-only); `cmux-shell` runs the executor segment: a side-effect-free call-time config re-read (startup owns the template write), run-shape checks ordered before any surface access (unknown name -> not_found; workspace flavor or unacknowledged confirm -> invalid_state naming the residual), then the send_text surface rules and `terminal.send(command + "\n")` (empty-after-sanitize reported; upstream silently no-ops).
  - CLI: `cmux command list [--json]` (rows print `name - description [confirm]  # command`, workspace flavor marked) and `cmux command run <name> [--surface <id|index|ref>] [--yes] [--json]` with strict per-verb option sets (unrecognized option = one-line error + exit 1, matching the browser refusal; reply `OK name=... surface=...`).
  - Recorded divergences: the confirm ack replaces upstream's dialog (scripts pass `--yes`), and the per-fingerprint trust ledger is out - the port reads one user-owned global file (per-project config overlays are future config work); the verbs re-read the config at call time instead of upstream's live store snapshot.
  - Verification: gates green (fmt, clippy -D warnings, 307 unit tests incl. the four new suites), `cargo build` fresh for both binaries (the shell changed this slice); smoke-cli.ps1 grew 40 to 49 checks (8 custom-command checks over a fixture config holding a plain echo, a confirm:true echo, and a workspace-flavor entry) and passed twice; full battery re-run on the binary change: smoke-m34-turn.ps1 PASS, test_automation_m4.py 16/16, test_automation_m42_password.py 13/13, test_terminal_ux_m43.py 19/19, test_pool_m44.py 8/8, test_browser_m51.py 99/99 x2 (after a harness native-focus settle fix, test-strategy section 14).

- **2026-10-22 - M6 warm-up landed: the `cmux browser` CLI wrappers; ADR-0006 Amendment 6.**
  - Grammar mirrors upstream `CLI/cmux.swift` runBrowserCommand (~19460-20135): scoped verbs take the surface first (`browser <surface> <verb> ...`) or --surface; the handle-free family (open/open-split/new); wait options --selector/--text/--url-contains|--url/--load-state/--function with --timeout-ms|--timeout; the click family + aliases, the key family, the get fork (url/title/text/html/value/attr/count/box/styles), the is fork (visible/enabled/checked), eval, select --selector --value, scroll --dx/--dy, screenshot --out; strict unknown-flag rejection; --json accepted anywhere; plain and json printing per upstream.
  - Budgets: waits use max(15 s, waitMs+9 s) - +1 s CLI grace over the app's waitMs+8 s pump park so a deadline always surfaces as the app's structured timeout (recorded divergence; upstream uses waitMs+8 s inside a max(15 s, ...) socket budget); other verbs keep 500 ms (the app enforces 400 ms first).
  - Targets and surfaces: @eN routes to the wire ref key, tokens stay CSS (ref-XOR-selector wire); the upstream positional-handle rule including its misroute is mirrored (a slot-0 scoped verb becomes the handle and answers the grammar error); unsupported upstream verbs answer a plain not-supported rejection with the docs pointer - verb slot only (unit-locked); refused-unbuilt flags (--id-format/--workspace/--window/--focus/--profile) rejected.
  - Two CLI bugs caught live by the new smoke section and fixed: the wait arm never sent the wire timeout_ms (deadlines silently defaulted to 5000 ms), and the unsupported pre-scan verb-scanned argument slots (a CSS selector spelling like an upstream verb got rejected); the smoke section and a unit test lock both regressions.
  - Live evidence: smoke-cli.ps1 extended to 40 checks (8 M4.1 + 32 browser over a file:// fixture: open grammar, navigate + wait ready poll, url/title/eval, snapshot refs, click by ref, fill, focus + press Enter observed in the page key log, select, scroll, is visible/checked, get count, predicate wait + structured timeout, screenshot --out and plain, focus-webview + is-webview-focused, history back, grammar negatives) PASS twice; battery re-run for the binary change - smoke-m34 PASS, test_automation_m4.py 16/16 (the baseline identify now polls like its ping; the app-plane startup wedge measured bounded ~1.4 s, test-strategy section 13), m42 13/13, m43 19/19, m44 8/8, m51 99/99; fmt/clippy clean, 299 unit tests; cmux-shell.exe unchanged (M5.4), cmux.exe rebuilt.

- **2026-10-22 — M5.2 CDP automation landed: the P0 subset through
  the deferred-reply completion plane; ADR-0006 Amendment 3.**
  - Verb surface (`cmux-ipc`): five new app-plane requests with
    payload-budgeted free text and `@eN` ref normalization (+4 parse
    tests); `execute_ipc_call` keeps the automation arms unreachable
    (the exhaustiveness arm documents the deferred-completion routing).
  - Deferred-reply plane: `serve_ipc_calls` validates + shapes + issues
    the first CDP call and parks `PendingCdp`; `cmux-webview2` grew
    `call_cdp` / `CdpCompletion` / `CdpTracker` / `FrameWakeFn` and a
    host-level completion outbox whose pump wakes the frame; the shell
    drains per host after each tick and runs `advance_cdp` (the
    `CdpStep` machine: Final / AfterCenter / AfterPress / AfterFocus).
  - Semantics: refs are snapshot-local and die on navigation (upstream
    ephemerality); eval answers `{"surface_id","value"}` with page
    exceptions as invalid_state; the snapshot installs the fresh ref
    table; click/dblclick/type lean replies.
  - Measurement made permanent: slow COM ticks (>5 ms, rate-limited) and
    slow CDP completions (>100 ms) with host-side round trips. Run
    evidence: lifecycle ticks only (5.1-17.2 ms), zero slow completions,
    client-side five-eval measurement min 10.5 / median 10.7 / max
    21.0 ms (app reply budget 400 ms).
  - ADR-0005 Amendment 5 executed: the harness client keeps one pipe per
    lifetime (handshake once per connection; transport-level failures
    reset it; parsed non-ok replies keep it).
  - Smoke hardenings (evidence-backed): create-with-verification for the
    startup-burst `workspace.create` timeout; foreground re-assertion +
    `foreground_is_app` detail beside the chord-gate checks after root-
    causing two consecutive ctrl+2 misses to environmental foreground
    theft (a live Firefox window claims foreground minutes into a run;
    SendInput refused 0/4 while it held it); two consecutive 60/60
    greens followed.
  - Verified: browser smoke 60/60 twice on the final binary, unit suite
    269, gates fmt / clippy `-D warnings` / test green; baselines re-run
    on the same binary: M4.1 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8,
    CLI 8/8, M3.4 turn PASS.
  - Remaining M5: the fill/press/hover/scroll/get.*/is.*/screenshot/
    find.*/frame.*/dialog P0 family, then CLI wrappers for the
    automation verbs; history readback snapshot-based; UDF-after-close
- **2026-10-22 — M5.3 landed: the observation + form/scroll family; ADR-0006 Amendment 4.**
  - Verb surface (`cmux-ipc`): `APP_METHODS` 20 → 42 — `browser.get.title`,
    `get.text/html/value/attr/count/box/styles`, `is.visible/enabled/checked`,
    `fill/focus/check/uncheck/select/hover/scroll_into_view`, `scroll`,
    `screenshot`, `focus_webview`, `is_webview_focused`; the element-verb
    target grammar is `ref` XOR `selector` (both flavors on every element
    verb incl. the M5.2 click/dblclick/type; ambiguity and absence are
    `invalid_params`; selectors ride as JSON literals under the payload
    budget); `browser.eval` additionally accepts upstream's `script` field
    name as a fallback (mutual exclusion enforced); capabilities serve the
    sorted union of both method tables (`system.capabilities` now advertises
    the browser families; the M4.1 smoke's loose shape check still passes).
  - Shell plane (`cmux-shell`): one uniform `CdpKind::ElementJs` one-evaluate
    plane for the new element verbs (JS per verb; the action parse maps
    `not-checkbox`/`not-select`/`no-option` sentinels to `invalid_state`;
    missing element on probes reads as `false`), hover = fresh-center step +
    one `Input.dispatchMouseEvent` `mouseMoved` (`AfterHoverCenter`), scroll
    = `window.scrollBy` + offset reply, screenshot = one
    `Page.captureScreenshot` decoded with the new base64 dependency (added
    to cmux-shell only, MIT OR Apache-2.0) and staged to
    `%TEMP%\cmux-screenshots\cmux-<surface>-<cdp-id>-<epoch-ms>.png`
    (reply carries the path; smoke verifies the real PNG magic); `get.title` rides the sync
    lane (tick-polled title, like url.get); `focus_webview`/`is_webview_focused`
    are model-plane verbs (activate workspace + set_active_focus → next
    tick's programmatic MoveFocus; focused = active workspace's focused leaf;
    both reject terminal surfaces with invalid_state).
  - Harness: the persistent client gained the M5.3 wrappers (`browser_fill`,
    `browser_check/uncheck`, `browser_select`, `browser_hover`,
    `browser_scroll*`, `browser_get/is`, `browser_get_title`,
    `browser_screenshot`, `browser_focus_webview`,
    `browser_is_webview_focused`) with `ref=`/`selector=` kwargs mirroring
    the wire; the M5.2 click/type wrappers accept the selector flavor too;
    the smoke's PAGE_FORM fixture grew the checkbox/select/hover/hidden/
    tall-scroll elements (ref table now @e1..@e6, existing assertions
    updated) and the automation section asserts every new family end to end.
  - Run evidence: browser smoke 87/87 twice (plus two diagnostics fixed on
    the way: the final chord press re-asserts app foreground after the
    recorded Firefox-theft pattern, and the scroll check asserts the
    vertical contract + horizontal stability — the pane's page carries a
    constant 8 px Chromium viewport-rounding sliver a dy-only scrollBy must
    not be blamed for); eval round trips min 10.6 / median 10.8 / max
    21.3 ms; unit suite 282 (ipc 40 → 46, shell bin 42 → 49); gates green;
    all six baselines re-run on the same final binary (M4.1 16/16, M4.2
    13/13, M4.3 19/19, M4.4 8/8, CLI 8/8, M3.4 turn PASS).
  - Remaining M5 (P0 close in M5.4): press/keydown/keyup (key-name→VK table
    → Input.dispatchKeyEvent) and wait (poll lane); then the P1 family
    (find.*, set_input_files, frame/dialog/console) and the CLI wrappers.

    decision stands (keep).

- **2026-10-22 - M5.4 landed: key synthesis + predicate waits; ADR-0006 Amendment 5; the spec's P0 automation list is closed.**
  - Verb surface (`cmux-ipc`): `APP_METHODS` 42 to 46 - `browser.press`, `browser.keydown`, `browser.keyup` (params `{"surface_id","key"}`, no selector - keys drive the focused element; upstream wire verified at CLI/cmux.swift 19668-19671) and `browser.wait` (predicates `selector|text_contains|url_contains|load_state(2 states)|function`, `timeout_ms` default 5000 cap 120000; upstream wire verified at CLI/cmux.swift 19455-19515 incl. the CLI's socket-headroom comment); `RequestProblem::wait_timeout` (code `timeout`, deadline ms in the message) joins the problem constructors (+2 ipc parse tests, 46 to 48).
  - Shell plane (`cmux-shell`): the key vocabulary is a port-defined Playwright-style table (~82 rows: named keys + the four modifier keys, letters/digits with US shift partners, 11 US punctuation rows with VKs 0xBA-0xDE; lowercase aliases; single-character spellings; uppercase/shifted punctuation imply Shift; `+` and a bare space special-cased around the `+`-split). `Input.dispatchKeyEvent` mapping: printing keys' down = `keyDown` carrying the resolved char (Ctrl/Alt/Meta suppress it), non-printing = `rawKeyDown`, up halves = `keyUp`; press = down then the prepared up (`CdpStep::AfterKeyDown`), keydown/keyup = one event. Waits poll one combined evaluate (slots in documented order; literals JSON-encoded, the function predicate verbatim at eval's trust level) - a false poll re-arms the same evaluate (completion-driven, no spin), a deadline hit answers the structured timeout, malformed replies fail fast `invalid_state`; the pending sweep anchor moved from `started` to `last_activity` (refreshed per re-arm) and the wait's per-call reply budget extends to deadline + 8 s headroom (upstream's CLI formula) while every other call keeps 400 ms.
  - Harness: the client gained `browser_press/keydown/keyup/wait` (waits get their own per-call timeout = deadline + 12 s); the smoke fixture grew the non-interactive `#keyLog` div + document keydown/keyup loggers (refs stayed @e1..@e6); the automation section adds the M5.4 family (Backspace editing, Control+a select-all with the modifier bit visible in the page's log, Enter's full pair, Shift halves in order, wait resolving selector/url/AND/function predicates + the structured timeout + unknown-vocabulary and terminal-surface negatives, 87 to 99 checks).
  - Run evidence: smoke 99/99 twice on the final binary; unit suite 290 (ipc 46 to 48, shell bin 49 to 55); gates fmt / clippy `-D warnings` / test green; all six baselines re-run on the same binary (M4.1 16/16, M4.2 13/13, M4.3 19/19, M4.4 8/8, CLI 8/8, M3.4 turn PASS). Two battery lessons recorded in test-strategy section 12: a smoke check contradicting an isolated probe meant the smoke's own body had drifted (the missing Enter press), and lingering window-less survivors made the M4.1 baseline time out twice at its first app-plane call until killed (server-plane ping answered, identify did not).
- **2026-10-21 — M5.2 residual slice landed: chord modifier gate, focus
  unification, teardown attribution; ADR-0006 Amendment 2.**
  - Modifier gate (`port/crates/cmux-webview2`): the accelerator router
    decides through `routes_as_chord(ctrl_held, virtual_key)` with
    `ctrl_held` read from `GetKeyState(VK_CONTROL)` INSIDE the COM
    callback — it runs on the UI thread during input processing, so the
    read matches the event being dispatched. The WebView2 accelerator
    contract still fires events for plain/alt digits; the gate just stops
    treating them as chords. Smoke negatives: a plain digit and an
    alt+digit pressed while the webview child owns the keyboard never
    switch workspaces; positive: ctrl+2 still routes the identical path.
    Unit-tested `routes_as_chord` combos (registered key + held Ctrl).
  - Click claims: `render_pane` reports explicit pane clicks through the
    new `clicked_now` channel (terminal widget and browser interceptor);
    the model-focus reconcile is now the pure, unit-tested
    `focus_reconcile_target` (claim > transition > nothing), so clicking
    the egui-focused terminal while a browser leaf holds model focus
    retakes model focus — the M5.1 accepted corner is closed (+3 shell
    tests for the corner states).
  - GotFocus routing: the controller's `add_GotFocus`
    (`webview2-com::FocusChangedEventHandler`) queues the pane id (the
    `BrowserSpec` grew `surface_id`); the shell drains it after the chord
    queue, updating model focus only for leaves of the ACTIVE workspace
    not already focused (LostFocus deliberately inert — blur never
    deselects). App stderr shows `accelerator captured: focus -> p3`
    (webview gained native focus) on programmatic restores; repeat
    arrivals no-op silently.
  - Egui-focus surrender (found through the new evidence channel): the
    pre-fix logs carried a phantom `focus -> p1` right after EVERY
    workspace re-select — egui's memory re-installs a remounted widget's
    past focus and the transition reconcile followed it although the OS
    keyboard sat in the webview child. `render_pane` now surrenders any
    standing egui focus whenever a browser leaf owns the model (real
    clicks bypass through the claim; `set_active_focus` re-arms the
    terminal's pending focus, so a click-claim recovers its widget's
    keyboard within one frame). Post-fix run: zero `focus -> p1` lines.
  - Teardown attribution (harness hardening): the smoke's process-exit
    check moved from a machine-wide msedgewebview2 count delta to exact
    pid identity — `Win32_Process` command lines containing the
    per-surface udf path; the owned set captured before `WM_CLOSE` must
    empty afterward. The single count failure this session was ambient
    churn (±1 foreign instance against an 11-foreign baseline); the
    marker-based check is churn-immune and strictly stronger.
  - Harness lessons (test-strategy §8): the smoke's final stderr-tail
    dump is now encoding-proof (a cp1252 console plus U+FFFD in app
    stderr killed one run's tail dump); `smoke-cli.ps1`'s `identify
    --json` gained the py-harness retry — the FIRST app-plane call after
    spawn can miss the 400 ms reply budget inside the shell-startup PTY
    burst (measured 478 ms in that window vs 11-15 ms once settled;
    M4.1-era behavior, load sensitive, app semantics unchanged).
  - Verified: browser smoke **43/43 PASS** (39 M5.1 checks + 3 gate
    negatives/positive + 1 udf-attribution check, incl. the restart
    section), unit suite 260 (+2 webview2 gate/focus-drain tests, +3
    shell reconcile-corner tests), gates fmt / clippy `-D warnings` /
    test green; all baselines re-run on the final binary: M4.1 16/16,
    M4.2 13/13, M4.3 19/19 (one unrelated SendInput flake retried green),
    M4.4 8/8, CLI 8/8, M3.4 turn runner PASS.
  - Remaining M5.2: CDP automation subset (feature row 18) with the
    persistent-socket client shape folded into its design; non-blocking
    COM operations vs the reply budget (hypothesis-level: needs a
    frame-budget measurement under load); history readback unchanged;
    UDF-after-close decision stands (keep).

- **2026-10-21 — M5.1 landed: browser panes end to end — WebView2 substrate
  hosted by the frame loop; ADR-0006 Amendment 1.**
  - Substrate (`port/crates/cmux-webview2`): `BrowserHost` lifecycle
    `EnvRequested → ControllerRequested → Wired` promoted inside per-frame
    `tick(TickParams)` (no blocking waits on the frame thread; completion
    closures park in the controller slot), per-surface user data folder
    `%LOCALAPPDATA%\cmux\webview2\<pane-id>` (kept on disk after close —
    documented residual), `HostPhase::Failed` paints the reason in the pane.
    `BrowserSnapshot` carries url/title/phase/canGo* read back via 20 ms
    throttled COM polls.
  - Keyboard route (the M5.1 centerpiece): `add_AcceleratorKeyPressed`
    registered on `ICoreWebView2Controller` (bindings 0.39.1 + Microsoft
    reference both place it there — verified, not assumed), matching VKs
    (shell-registered 0x31..0x39) get `SetHandled(true)` + queue +
    `ctx.request_repaint()` wake; `ui()` drains between `process_bindings`
    and `drain_actions` and maps digits through `action_for_accel_key`.
    Smoke-proven both ways: egui route (keys land on the main window) and
    router route (native focus inside the covered webview child — egui
    never sees them; the app log shows event → captured → drained →
    switched).
  - Root-caused fix along the way: the M2.x "model focus follows the
    keyboard" reconcile ran every frame and silently reverted a browser
    leaf's model focus to the standing egui-focused terminal widget
    (identify said p1 one frame after `focus -> p3` with no focus log).
    The reconcile now follows only egui focus transitions; diagnostics
    added permanently (`focus ->`, `ipc serve <id> <method>` via
    `AppRequest::method_name()`, unfiltered `accelerator event:`).
  - Persistence decision: `browser.navigate` always updates
    `lane.start_url`, so the saved session reopens the pane's LIVE page
    (smoke restart: restored browser reloads the saved two.html, identify
    replays the exact saved focused pane id from session.json).
  - Verified: `port/harness/test_browser_m51.py` **39/39 PASS** (open/readiness/
  navigate fan/error gates/process counts/geometry pin/UDF/chords in both
    routes/hidden-pane survival/teardown + process exit/session snapshot/
    restart restore/panic sweep); unit 255 with +8 (shell bin 3: the
    digit-map, per-surface-UDF nesting and defer-host/title assertions;
    webview2 3: DIP-rect rounding pair + the router match/drain pairing;
    ipc 2: browser-verb parse + URL-violation tests); all six
    earlier baselines re-run on the same binary (M4.1 16/16, M4.2 13/13,
    M4.3 19/19, M3.4 9/9, CLI 8/8, pool 8/8 twice); gates fmt / clippy
    `-D warnings` / test green.
  - Residuals for M5.2 (ADR-0006 Amendment 1): CDP automation subset
    (feature row 18), digit-chord modifier gate, model/native focus
    unification (GotFocus/LostFocus + surrendering egui focus for browser
    leaves; one accepted corner recorded), non-blocking COM ops vs the
    400 ms reply budget, UDF clean-up decision.

- **2026-10-21 — M4 audit close-out: M4 declared machine-complete; owner
  decisions recorded; owner manual matrix handed over.**
  - Completion audit against the plan row (scope "IPC server + CLI subset +
    Python harness"; acceptance "upstream harness pattern drives the port's
    named pipe; protocol tests green"): both clauses verified (harness
    live-smokes 16/13/8/8 across M4.1/M4.2/M4.4 on the current binary, 247
    protocol/unit tests green). Deliberately-out scopes confirmed still
    documented, not lost: `allowAll` + HMAC capability tokens (no consumer /
    no Windows analog, ADR-0005 Am.3), `surface.type`/selection verbs +
    browser methods (Post-MVP / M5).
  - New verified fact from the audit: upstream's harness client holds one
    persistent socket per client instance (`tests_v2/cmux.py`
    `connect()`/`close()`); the port's harness opens one connection per
    call. **Owner decision (2026-10-21): divergence accepted for M4; the
    persistent-socket client shape folds into the M5.2 design** (ADR-0005
    Amendment 4 divergence bullet updated; feature-parity row 16 + M4 row
    risk cell updated).
  - Owner acks recorded in the ADR headers: **ADR-0012** (Windows font
    chain) and **ADR-0013** (no forced `TERM`/`COLORTERM`) — both ratified,
    the M4.3 report's open acknowledgements cleared.
  - Owner manual-verification matrix (`docs/testing/owner-manual-matrix.md`)
    handed to the owner for hands-on rows 1/2 (carried since M2.4b) + the
    M4.3 terminal-feel rows; findings route through the normal
    bug-fix-first workflow. Machine lanes stand as the record meanwhile.

- **2026-10-20 — M4.4 landed: concurrent connection pool — the deferred M4
  item closed; ADR-0005 Amendment 4; owner manual-verification matrix.**
  - Upstream evidence (`docs/cli-contract.md` §"Control-socket admission and
    deadlines"): the app never lets one control command block the others;
    every accepted connection gets a real reply; saturation answers the
    structured `overloaded` error. The port's until-now shape — one pipe
    instance (`nMaxInstances = 1`, accept → serve → disconnect loop) —
    violated both: all connections serialized, and a connected-but-silent
    client parked the whole server indefinitely.
  - Port shape (`cmux-ipc/server.rs`): the pipe name holds 8 byte-mode
    instances, each accepted and served on its own thread; per-connection
    semantics unchanged (auth handshake, rotation checks, ancestry gate —
    all per connection). The name claim (`FILE_FLAG_FIRST_PIPE_INSTANCE`)
    happens on the main thread before any worker spawns, preserving the
    second-app-instance `IPC disabled` path (workers created before the
    claim would otherwise join a foreign pipe). Handler/provider shared
    across threads as `Arc<dyn Fn … + Send + Sync>`; no new locks; the app
    request plane (frame-loop drain) is untouched.
  - Recorded divergence: saturation sits at the OS open (`ERROR_PIPE_BUSY`)
    with the client's connect-budget retry, not an in-band `overloaded`
    reply; the bounded-claim residual (a same-user peer can hold all 8
    slots) mirrors upstream's own pool capacity, not a new hole.
  - Verified: +2 pool unit tests (`a_parked_request_does_not_hold_the_pipe`;
    `every_pooled_connection_receives_its_own_reply`) = **247 tests**; live
    smoke `port/harness/test_pool_m44.py` 8/8 PASS twice (32 pooled
    exchanges; CLI ping in 0.01–0.05 s while a silent peer holds a slot;
    same-name second instance logs `IPC disabled` while the first serves);
    all baselines re-run on the same binary: M4.1 16/16, M4.2 13/13, CLI
    8/8, M3.4 9/9, M4.3 terminal-UX 19/19; gates fmt / clippy `-D warnings`
    green.
  - Also landed: `docs/testing/owner-manual-matrix.md` — the owner-facing
    L5 checklist (launch recipe + workspace/tab/split/terminal/IME/
    notification/persistence/config/CLI actions with expected results),
    referenced from `test-strategy.md`.

- **2026-10-20 — M4.3 landed: terminal-UX close-out — driver-lane rows 5/6/7
  over real input (battery + product fixes); ADR-0012 (fonts), ADR-0013
  (pane env).**
  - Battery: `port/harness/test_terminal_ux_m43.py` 19/19 PASS, twice on
    the final binary: launch/ping, fonts log line, window found, foreground,
    `system.identify` focus carry, click-focus keep, typed markers visible,
    arrow-up recall, Ctrl+C interrupt, drag selection → clipboard,
    Ctrl+V paste, unicode-only CJK burst (wide-cell normalization),
    long-output viewport settle, wheel up/down, resize hammer, WM_CLOSE
    exit, session file written.
  - Probe-coordinate root cause: the app's chrome measurements are egui
    logical points (sidebar 216) while SendInput pushes physical screen
    pixels; at 1.25× DPI every "mid-pane" press landed on the sidebar where
    `contains_pointer()` is false, so no selection ever formed. The whole
    failed-drag era (including the "multi-row drags don't hold a selection"
    observation) is explained by this one artifact — verified by a
    post-fix multi-row drag that selects and copies 5 rows.
  - Input-technique hardening recorded in `test-strategy.md`: per-frame
    event collation (click down/up separation, drag dwell pacing), US-layout
    pin per window (HKCU preload can carry an Arabic layout remapping `C`),
    clipboard poll semantics (lazy writes; the ghost-marker signature).
  - Product change (ADR-0013): `shell_spec` no longer forces
    `TERM`/`COLORTERM`. With `TERM` set, the Ctrl+C interrupt in pwsh under
    ConPTY stops matching the Windows-Terminal contract (the recalled line
    survives the chord instead of `^C` + fresh prompt); without it, parity
    holds (mechanism: PSReadLine's TERM-gated raw-VT input — strong
    inference; the behavior difference itself verified both directions).
    The battery's `^C`-interrupt check pins the regression.
  - Product addition (ADR-0012): `cmux-shell/src/fonts.rs` installs the
    machine's Windows faces into egui at eframe creation (Cascadia Code →
    Cascadia Mono → Consolas primary; YaHei/JhengHei/MS Gothic/Malgun
    CJK coverage chain; silent default degrade + startup log), 7 unit
    tests behind a pure planner/composer.
  - M3.4 smoke fixture hardened (focus pin): the regression re-run's
    `focused-inline`(=`desktop=false`) delivery was the plain-launch
    foreground race — the OS occasionally focused the new window before the
    delivery instant and the (correct) arrival policy responded. The smoke
    now pins the foreground to the shell desktop after the app window
    appears (ALT-trick + `SetForegroundWindow`); 3/3 PASS after the fix.
    Smokes moved to the repo for durability:
    `port/harness/smoke-m34-turn.ps1`, `port/harness/smoke-cli.ps1`.
  - Gates: fmt / clippy `-D warnings` / cargo test green — 245 tests
    (+7 fonts); baselines re-verified: M4.1 harness 16/16, M4.2
    password 13/13, CLI 8/8, M3.4 turn 9/9 (3× post-fix).

- **2026-10-20 — M4.2 landed: auth modes + DPAPI credential store (code +
  enforcement tests + live smokes); ADR-0005 Amendment 3; owner security
  review ratified (§12 record in the ADR).**
  - Upstream evidence: only `.password` requires the handshake
    (`SocketControlMode.requiresPasswordAuth`); password sources are
    explicit → `CMUX_SOCKET_PASSWORD` env → state-dir file
    (`SocketControlPasswordStore`); verification re-reads the store per
    check with a full-byte constant-time compare; the connection keeps a
    SHA-256 fingerprint and is revoked when it stops matching
    (`SocketPasswordAuthorization`); V1/V2 login reply texts and error
    codes captured verbatim (`ControlClientLineReaderTests` line 525 pins
    the envelope; `cmux.swift` CLI resolve order + `Unknown command
    'auth'` tolerance).
  - `cmux-ipc/password.rs` (new): resolve/normalize, constant-time compare,
    CNG SHA-256, DPAPI protect/unprotect + file store at
    `%APPDATA%\cmux\socket-control-password` (`CMUX_PASSWORD_FILE`
    override; corrupt reads as unconfigured). `AuthMode` moved into
    `ServerConfig` (`spawn_config`), the legacy `spawn_server` maps
    `require_descendant` to `CmuxOnly`/`Automation` (old behavior
    preserved; `CMUX_SOCKET_PATH`-override ancestry-off = `automation`).
    `serve_one_client` became a multi-line loop (the auth+command
    connection shape); rotation check per served command disconnects a
    stale-fingerprint connection.
  - `cmux-shell`: `CMUX_SOCKET_MODE` env (exact upstream values, unknown
    warns + default, `off` disables); provider resolves live per check
    (`default_password_provider`); mode logged, secrets never logged.
  - `cmux-cli`: global `--socket`/`--password` (space and `=` forms, any
    position) via an `OnceLock<Globals>`; `make_request` sends the V1 auth
    probe with the CLI tolerance when a password resolves.
  - Harness (`cmux.py`): `password=` login before every call on the same
    connection; `test_automation_m42_password.py` live smoke (13/13 PASS
    first run): password-mode refusals/enforce (`auth_required`,
    `auth_failed`+text, `invalid_params`), authenticated client +
    capabilities listing `auth.login`, CLI triangle (wrong→exit 1 "Invalid
    password", right→exit 0 pong, no-source→exit 1), automation mode (serves
    unauthenticated, `auth.login` ack `required:false`, CLI `--password
    ignored` + password-less CLI both exit 0).
  - Gates: fmt/clippy `-D warnings`/`cargo test` green — 238 tests (+15:
    8 password-store, 7 server-level auth enforcement incl.
    rotation-refusal, V1 texts, non-password ack, ancestry independence,
    CLI tolerance); M4.1 harness 16/16, CLI smoke 8/8, and the M3 battery
    9/9 re-verified against the rebuilt workspace.

- **2026-10-20 — M4.1 landed: automation surface + app request plane +
  Python harness (code + tests); ADR-0005 Amendment 2.**
  - `cmux-ipc`: method surface grown to the v2 subset (`system.ping`,
    `system.capabilities`, `system.identify`, `workspace.list/current/select/create`,
    `surface.list`/`pane.list`/`surface.send_text`/`surface.read_text`) with
    `AppRequest` parse/validate, `MethodRoute` dispatch, and structured
    error/timeout envelopes; the handler widens to
    `Fn(IpcCall) -> Option<Result<Value, RequestProblem>>` (enqueue keeps
    its `queued:true` fail-open semantics; app calls are forwarded to the
    frame loop and answered there).
  - `cmux-shell`: per-frame app-call drain (`serve_ipc_calls` inside
    `poll_children`, server thread parked per call with a 400 ms budget kept
    under the CLI client's 500 ms read budget); `workspace.create` reuses the
    exact `Action::NewWorkspace` path (extracted `spawn_new_workspace`);
    `surface.send_text` types through the terminal writer, `surface.read_text`
    reads the visible grid (screen-only, documented); row shapes mirror the
    upstream harness's consumers.
  - Identity: process-global pane-id mint (`p1` first, unique across
    workspaces — repairs the pre-existing bug where every workspace reused
    `p1`, breaking by-surface hook routing); workspace ids `w<n>` minted and
    carried in snapshots (upstream uses UUIDs — documented divergence);
    restore seeds both counters above every restored id.
  - `cmux-cli` verbs: `identify`, `workspace list|new|current|select`,
    `surface list`, `send [--surface <spec>]`, `read-screen` (CLI smoke
    8/8 PASS against a live app).
  - `port/harness/cmux.py`: stdlib-only ctypes named-pipe client (64-bit-safe
    handles, deadline-bounded reads) mirroring the upstream harness API
    subset; `port/harness/test_automation_m4.py` live smoke: 16/16 PASS on
    the first end-to-end run (spawn → ping → capabilities → identify →
    list/create → rows → typed marker visible → select → error shapes →
    teardown).
  - Gates: fmt clippy cargo-test green (223 tests across 22 suites, +12 over
    M3); **M3 battery re-run 9/9 PASS** — the four turn variants initially
    failed due to pre-existing smoke-fixture defects (stale done-mark,
    runner marking the parent directory, placeholder emitted via `echo`
    instead of a live prompt line), root-caused and fixed in the temp
    scripts; a causal re-test confirmed app-side delivery was never at fault
    (PASS with the base runner template, including a restored session).

- **2026-10-19 — M3.4: prompt-line turn detection landed (code + unit
  tests); NEW pre-existing pty stream defect discovered and scoped.**
  Uncommitted working-tree per this milestone's convention; owner review
  pending alongside the rest of M3. See ADR-0011 Amendment 6.
  - Scope decision: port the local Swift `PromptLineTurnDetector` only;
    the herdr Rust screen-scan plugin stays Post-MVP. Builtin table =
    Ollama only (`">>> "` / `"Send a message (/? for help)"` / 500 ms),
    byte-equal to upstream's builtins (parity test).
  - Clean-room Rust reimplementation in
    `port/crates/cmux-shell/src/turns.rs` (Swift app code is GPL-3.0):
    upstream's 17 detector tests ported 1:1 with byte-exact inputs, plus
    fragmentation, config-rejection, 5 runtime tests (tee-style
    deadline-passed-boundary rule, pending supersede/invalidate,
    exactly-once), a ConPTY 255-byte resize-repaint regression that
    completes instantly, an emulator-reply exoneration test
    (`emulator_replies.rs`: the exact captured chunk stream produces zero
    write/reply/resize calls from egui_tty), and the full smoke hand-over
    sequence through a real `TurnRuntime`
    (`smoke_handover_sequence_announces_exactly_once`). `cargo test` 209
    green (+30), fmt + clippy `-D warnings` clean.
  - Wiring (testability refactor first): `start_shell`, `Pty`, `PaneEvent`,
    and `WatchedStatus` moved from `main.rs` into the lib module
    `cmux_shell::shell_session` so the probe examples and unit tests can
    drive the real spawn/reader code; the app keeps only the egui frame
    loop. Then the wiring: `start_shell` reader emits `Vec<PaneEvent>`
    (`Notifications` | `Turn`); `Pane` gains `pending_turns` /
    `delivered_turns`; the frame loop drains in `poll_children(ctx)`
    (supersede via `is_none_or`, repaint at deadline via
    `request_repaint_after`) and a deadline sweep delivers through the
    shared policy pipeline (ring + toast + decision log) exactly once per
    agent turn. Divergence: no foreground-PID check (ConPTY has no
    fg-process-group query); guards = pane-child liveness + per-agent
    high-water mark.
  - **Defect chain resolved 2026-10-19 (live smoke PASS verified 3x).**
    Two root causes behind "stream healthy but no turn-complete":
    1. *Frame-wake gap (fixed):* `TurnRuntime::consume`'s confirm path
       deliberately emits no event (upstream delivers from the app's
       deadline sweep), but nothing woke the egui frame loop while it was
       idle, so pending turns announced from reader threads were never
       observed. Fix: `start_shell(spec, wake)` takes a `WakeFn`
       (`Arc<dyn Fn() + Send + Sync>`) called after every channel send;
       the app installs a `FrameWake` (Mutex-held `egui::Context`,
       installed on the first frame via `App::ui`, shared by all spawn
       paths) that calls `request_repaint()`. The lib stays egui-free;
       probe examples pass no-op wakes.
    2. *Emitter semantics (smoke corrected + pinned):* a
       line-terminated waiting prompt can never announce a turn — the CR
       that closes the line immediately invalidates the pending the
       accumulation opened (`handle_line_boundary` runs before
       `resetLogicalLine`). Upstream waits for the prompt to paint and
       sit, unterminated. The smoke emitter now reproduces a real Ollama
       hand-over: idle prompt + CRLF (arms) → submission echo → response
       → idle prompt **without CRLF** (pending opens, deadline sweep
       delivers at +500 ms) → late `done` chunk (tee-rule pre-confirm,
       exactly-once). Pinned by
       `smoke_handover_sequence_announces_exactly_once`.
    - Verification: the lib probe ran the exact runner through a real
      ConPTY and announced `Pending(delay=500ms)` live at 2.06 s;
      smoke runs produced `agent turn-complete ollama in workspace 1 →
      external ring=true flash=true desktop=true` exactly once (plain
      smoke 2/3 of a small battery, focus variant deterministic). Both
      probe examples updated to the new `start_shell` signature.
  - **Residual defect (tracked, pre-existing, unrelated to turn
    detection): nondeterministic pane-child freeze.** In a minority of
    fresh-app launches the pane's shell's *child processes* freeze
    forever mid-sleep: forensics show `ping.exe`/`timeout.exe` alive at
    kill time tens of seconds past their own 1–2 s sleeps (`timeout /t 2`
    alive at +33 s), created on time ~1–2 s after the pane's cmd; the
    app's reader is healthy (heart-beat tap shows it parked in `read()`
    waiting for data the frozen child gates); the runner's crumb log
    stops between steps. Not a detector/emulator/IPC/toast/spawn issue
    (all exonerated by probe or knob). Correlation pattern: probe-style
    lifecycles (app self-exits orderly after the emitter finishes) ran
    clean 12/12; the smoke's rapid relaunch cadence (force-killed live
    tree → 2 s gap → next spawn) wedged ~7/11 regardless of window
    visibility knobs (hidden windows can't be AppActivate'd — untested
    correlation). Open questions for its own slice: ConPTY/console-session
    teardown-after-force-kill vs immediate respawn; whether production
    (user-managed, non-rapid-restart) panes are affected at all. Dev taps
    kept: `CMUX_DEBUG_READER` chunk hex + read heartbeats, runner crumb
    log, smoke fail-time process-tree dump.

- **2026-10-20 — M3 defect chain closed: pane-child freeze root-caused
  (fixed) + smoke-fixture bug fixed; 12/12 battery PASS; M3 fully landed.**
  Owner's direction: use cmux-original as the reference and REA for
  evidence; no commit (full-port working tree, per owner - see §16 note).
  - *Upstream reference read (source, cmux-main):* upstream owns every
    spawned child subtree with an OS container and an ordered teardown
    chain - `AgentForkCommandOutputRunner.swift` (child-led process group +
    `POSIX_SPAWN_START_SUSPENDED`, exit watched before `SIGCONT`, TERM+KILL
    the whole group while the zombie still pins the pgid, independent
    reaper), `AgentHookDeliveryProcess.swift` (setpgid + TERM -> grace ->
    KILL escalation), and the pane-close path routes through the surface
    provider. Windows analogue: job objects - which ADR-0003 pre-declared
    in its own Decision section.
  - *Implementation:* per-pane `PaneJob` in `cmux-pty-port`
    (`KILL_ON_JOB_CLOSE` + breakaway denied, assigned immediately after
    `spawn_command`; parked through `PtyParts` into the session-lifetime
    `Tty` holder; noisy degrade to the uncontained spawn if containment
    fails; `tree()`/`terminate()` for the explicit hard-kill stage). Two
    live ConPTY tests: a descendant survives its leader's termination while
    the session is alive and collapses at session drop; `terminate()`
    empties the tree.
  - *Root cause 1 (the freeze itself):* the ~7/11 rapid force-kill cadence
    wedge came from trees with no kernel owner (children can outlive a
    TerminateProcess'd ConPTY host chain and stall mid-command). Post-job
    batteries: `timeout.exe`-style mid-sleep stalls vanished completely
    (8/8 headless + 5/5 timing + final 12/12, all <=6 s runner completion).
  - *Root cause 2 (the misleading metric):* the smoke never cleaned
    `done-mark.txt` between runs. After the first passing run, every
    later run's wait loop exited instantly and read the app log ~2-3 s
    into the launch - racing the ~1.4-2.3 s delivery, so the checker
    occasionally read the log before the line existed ("expected 1, got
    0"). The `LATE:` re-read discriminator + `t=` delivery-stamp dev tap
    (under the existing `CMUX_DEBUG_READER` gate) split the modes; with
    per-run fixture cleanup the metric became deterministic. Delivery is
    in-band once per turn exactly as designed; nothing about detection
    itself was ever flaky.
  - *Acceptance:* 12-runs 12-passes smoke battery (clean fixture,
    stopwatch taps, per-run residue forensics showing no console-substrate
    residue correlation), `cargo fmt --check`/`clippy -D warnings` clean,
    workspace 211 tests green (+2 container tests).
  - *Docs:* ADR-0003 amendment (job-object containment landed), ADR-0011
    M3.4 amendment rewritten to the closed state, M3 table row moved to
    landed/closed. REA's debug attach was not needed: the wait reasons of
    stalled children were never sampled live because the containment fix
    ended the wedge before the interactive-observation round; the byte
    and process evidence above carried the diagnosis.

- **2026-10-19 — Zero-cost real-claude acceptance PASSED (owner-approved
  probes; closes Amendment 4's pending acceptance item).** Stacked
  zero-cost guarantees: blocking SessionStart probe hook,
  `CLAUDE_CONFIG_DIR` → fresh temp dir, no credential vars, dead
  `ANTHROPIC_BASE_URL` riding the app env into panes.
  - Probe 1 (probe doc): real claude 2.1.302 loaded the `--settings`
    document, executed its exec-form SessionStart hook (marker file
    written from claude's own hook spawning), then hit its own
    `/login` gate — no API call. `--settings` trust-gating unknown
    from Q6: **closed** (no skip on this box/build).
  - Probe 2 (app + temp env, runner pane types `call claude -p "hi"`
    through the M3.3c shim): the port's real generated doc applied with
    zero settings errors, and the app log recorded the full lifecycle
    through the pipe — `session-start`, `prompt-submit`, `stop`,
    `session-end` — each `external ring=true flash=true desktop=true`
    with real desktop toasts (the owner may have seen 4 notifications
    from the probe). Pane exit code 1 (claude's own auth exit)
    forwarded through the shim chain; pane env verified to carry
    `CLAUDE_CONFIG_DIR`/dead base URL all the way down.
  - Recorded in ADR-0011 Amendment 4 closure note + findings Q6/Q7
    updates; claude merges the legacy `%USERPROFILE%\.claude\settings.json`
    even with `CLAUDE_CONFIG_DIR` set (state-only redirect in 2.1.302 —
    read-only, irrelevant to the port's doc).

- **2026-10-19 — M3.3c: typed-claude PATH shim landed (ADR-0011
  Amendment 5).** Uncommitted working-tree per this milestone's
  convention; owner review pending alongside slice A + M3.3b.
  - Design mirrors upstream's `agentCommandShims` + wrapper: a shared
    shim dir (`%LOCALAPPDATA%\cmux\claude-settings\shims\claude.cmd`,
    CRLF, `%*` delegation — per-surface uuid dirs not needed with the
    port's deterministic pane ids, divergence documented), the
    `hooks claude launch` verb (doc storage + `--settings` injection
    when in a live pane, plain pass-through + exit-code forwarding
    otherwise, 127 when real claude is unresolvable; re-entry proof =
    shim-dir skip in a directory-major `.exe/.bat/.cmd` scan).
  - `cmux-hooks::claude`: `settings_dir`, `shim_dir`, `install_shim`,
    `path_with_shim` (idempotent prepend), `resolve_real_claude` —
    +3 unit tests. `cmux-cli`: `launch` verb wiring (+ usage note).
  - `cmux-shell`: `claude_shim_for_panes` gates at spawn (IPC on,
    disable var unset, sibling CLI present; failures log and keep the
    plain PATH); identity routes the pane PATH **explicitly** from the
    app's live PATH (shim slot prepended only when enabled) — smoke
    evidence that portable-pty composes an un-overridden child PATH
    from the default env block on Windows (`Path=` registry casing and
    content), losing app-side PATH mutations, while all other variables
    propagate; bare-name `claude` pane programs now bypass spawn-time
    injection to avoid double `--settings` (tests updated).
  - Live smoke PASS (temp env, hermetic fake claude + runner pane):
    typed `call claude --probe` reached the shim; fake argv = exactly
    `--settings <doc> --probe`; `exit=7` forwarded through
    shim → verb → fake → pane; disable var → no shim dir, `--probe`
    only, exit still forwarded; verb outside a pane pass-throughs
    unchanged. `cargo test` 178 green (+5), fmt + clippy clean. Real
    claude acceptance still owner-side; no API-spending probes.

- **2026-10-19 — M3.3b: Claude settings-document parity + spawn-time
  injection landed (ADR-0011 Amendment 4).** Uncommitted working-tree per
  this milestone's convention; owner review pending alongside slice A.
  - Evidence pass first (owner directive: "use REA to understand it").
    `docs/research/rea-findings.md` Q6 records it: REA bound
    `claude.exe` 2.1.302 (PE x86_64) but the decompiler-engine provider is
    unavailable on this box, so per the REA recovery guide the observations
    are labeled ordinary-tooling evidence — a direct byte-scan of the
    embedded JS bundle plus the official hooks reference plus upstream
    source reads. Verified: the full event vocabulary incl.
    `StopFailure`; `asyncRewake` as a native claude feature ("wakes the
    model on exit code 2 … implies async"); `--settings` merges
    *additional* settings; hook `timeout` in seconds; documented **exec
    form** hook handlers spawn "with no shell" (the Windows port shape);
    upstream's injected doc read verbatim from
    `CMUXCLI+ClaudeHookSettings.swift`.
  - `cmux-hooks::claude`: the settings-document generator — upstream's
    exact table (6 queued lifecycle groups, Stop's `feed` + async
    `auto-name`, `inbox-wait` async+asyncRewake 86 400 s on
    SessionStart/Stop/StopFailure, `inbox-drain` on prompt submit,
    PreToolUse `CronCreate` guard + queued `pre-tool-use`, PostToolUse
    `PushNotification`, PermissionRequest → `hooks feed --source claude`
    125 s, `preferredNotifChannel: notifications_disabled`) in **exec
    form** (absolute `.exe` + `args`, no shell — divergence 1, documented);
    compact sorted-keys serialization; content-addressed
    `cmux-claude-<sha256[:8]>.json` doc writer; `is_claude_program`
    stem matching. 5 tests incl. the golden-table test.
  - `cmux-hooks` catalog: port-side `CLAUDE` def (upstream has no
    `AgentHookDef` for claude — the def carries the disable var
    `CMUX_CLAUDE_HOOKS_DISABLED`, the never-written `settings.json`
    description, and the 6 queued events; feed/verb events ride the doc);
    ownership now covers `hooks enqueue claude …` and the verb form
    `hooks claude …` (upstream's legacy matcher owns all `cmux hooks
    <agent> …` verbs too); +2 tests.
  - `cmux-ipc`: `KNOWN_AGENTS` = `["codex", "claude"]` (+comment; the
    unknown-agent test now pins grok rejection and claude admission).
  - `cmux-cli`: `hooks claude inject-settings` (no-socket verb; bin path
    from `CMUX_CLAUDE_HOOK_CMUX_BIN` override or current-exe),
    `hooks feed --source <agent>` (admission-queue funnel), fail-open `{}`
    stubs for `inbox-drain|inbox-wait|auto-name|cron-create-guard` (M4+
    backends; async/non-decisional upstream so no stall/rewake), usage
    text extended, and the stale `cmux hooks setup` hint now names
    `cmux hooks codex install`.
  - `cmux-shell`: spawn-time injection at the single spawn choke point
    (claude program → write doc → append `--settings <doc>`) with gates
    (IPC on via pipe identity, `CMUX_CLAUDE_HOOKS_DISABLED=1` forwarded
    through `identity_spec`'s managed-env pattern when the app env
    carries it, restored `--settings` specs left alone, `cmux.exe` sibling
    required; every failure degrades to plain claude, logged). The
    notification drain gained the journal-route split: `feed` and
    `pre-tool-use` log with ring/flash/desktop = false (upstream keeps
    the tool-loop feed out of the notification policy); the log line now
    reports effective effects.
  - Live smoke (hermetic temp session/config/pipe/LOCALAPPDATA,
    claude.cmd probe as the pane program): doc
    `cmux-claude-29168d16.json` appeared and probe argv carried
    `--settings <that path>`; doc bytes == the CLI verb's output; pane env
    showed all three identity keys; `hooks enqueue claude stop` →
    `{"id":"hook","ok":true,"result":{"queued":true}}` exit 0 with the app
    logging `agent hook claude stop in workspace 1 → external ring=true
    flash=true desktop=true`; the disable var run kept `--settings` out of
    argv, wrote no doc, and forwarded the var to the pane; the stub verbs
    print `{}`. First attempt failed only on a wrong smoke-config shape
    (`terminal.shell` is a `{program}` object — the app's fallback+
    warning worked as designed). Real-claude acceptance (settings
    validation + hooks actually firing) = owner step; no API-spending
    probes were run.
  - Checks: `cargo fmt` clean, `clippy --all-targets -D warnings` clean,
    `cargo test` **173 green** (+9 this slice).
  - Next: ~~typed-claude PATH shim~~ → landed same day (M3.3c entry
    above), then turn detection and the uncommitted-code owner review /
    commit authorization.

- **2026-10-19 — M3.3 slice A: direct-CLI hook transport landed
  (ADR-0011 Amendment 3; ADR-0005 Amendment 1).** Uncommitted working-tree
  per this milestone's convention; owner review pending.
  - `cmux-ipc`: synchronous one-thread pipe server (DACled to the user SID,
    ancestry gate via Toolhelp walk, 1 MiB envelope / 64 KiB payload /
    0.5 s reply budgets, single-instance per name, `CMUX_SOCKET_PATH`
    override = ancestry-gate off as a smoke affordance); 11 tests incl.
    a live pipe round-trip and ancestry rejection with a real child process.
  - `cmux-hooks`: the codex installer engine — merge/prune with
    insertion-index re-inserting (upstream `insertCmuxHookValues` semantics),
    ownership/legacy matchers, canonical sha256 hook hashes (compact
    sorted-keys JSON; serialized differ from Apple byte layout by design),
    trust entries/keys/prefixes, and the marker-block `config.toml`
    editor (feature block + trust block + multiline-string masking +
    idempotent install; uninstall restores user content).
    28 tests; fresh-dist round trip byte-verified.
  - `cmux-cli` (binary name `cmux` — load-bearing for the ownership
    matcher): `ping` (exit 1 unless `ok:true`), `hooks enqueue`
    (spawn/ancestry gates client-side, always exit 0, fail-open `{}`,
    routed-env allow-list + parent-pid fallback), `hooks codex
    install|uninstall [--yes]` (plan preview + y/N; `config.toml` is never
    fabricated when missing).
  - `cmux-shell` wiring: the server handler pushes into an mpsc that
    `poll_children` drains through the same `policy::resolve` pipeline as
    OSC events (ring + toast + disposition log); panes get
    `CMUX_SURFACE_ID`/`CMUX_WORKSPACE_ID`/`CMUX_SOCKET_PATH` injected
    (`identity_spec`, including the restore path);
    `surface_focused` fixes the focused-surface regression (event-pane id
    compared against itself — every active-workspace pane claimed focus)
    with two unit tests pinning it.
  - Live smoke matrix (all on this machine): ping 14 ms pong; in-pane
    descendant ping/enqueue over the per-user default pipe with the
    ancestry gate ON; pane env dump proves all three injected keys;
    non-descendant ping rejected (`unauthorized_peer`, exit 1);
    non-descendant enqueue fail-opens (`{}`, exit 0); invalid JSON payload
    fail-opens; focused pane + unfocused app → `external` + ring + real
    toast posted; unknown surface → no ring + toast; app-focused event →
    `focused-inline`, no toast; installer round trip restored the owner's
    scratch `config.toml` byte-identically.
  - Divergences recorded (not hidden): inline hook command strings instead
    of spool scripts; admission gating in the CLI not the shell guard;
    feed events (7) not installed this slice (plumbing ready in the
    catalog); `config.toml` trust hashes port-internal only (no Windows
    codex trust oracle locally).
  - Checks: `cargo fmt` clean, `clippy --all-targets -D warnings` clean,
    `cargo test` **164 green** (+40 this slice); the smoke binaries were
    exercised manually (per-step evidence above) — the panes' stub probe
    (`probe.cmd`) is not part of the test suite.
  - Next: ~~M3.3b (Claude wrapper/spool transport)~~ → landed (M3.3b
    entry above; the wrapper transport became the settings-doc +
    typed-shim pair), turn detection, then the uncommitted-code owner
    review / commit authorization.

- **2026-10-18 — M3.2a: delivery policy landed (ADR-0011 amendment;
  M2 accepted + ADRs accepted).**
  - The owner accepted M2 in-session and directed the remaining Proposed
    ADRs to Accepted (0002/0003/0004/0007/0008/0009/0010/0011 flipped
    2026-10-18; 0005/0006 stay Proposed — they scope M4/M5 work that has
    not been built yet).
  - M3.2a = upstream's
    `TerminalNotificationDeliveryDecision` + `NotificationPolicyEffects`
    ported one-for-one (`policy::resolve`, `PolicyEffects`,
    `ArrivalDisposition` in `cmux-shell/src/policy.rs`): mute gate →
    focused-inline gate (banner/flash/reorder off, quiet unless
    `soundWhenFocused`) → external; `suppressWhenAppFocused` detour
    removes only the banner. Both settings default `false` upstream and
    are passed as such until M3.2b adds config keys.
  - Wire-up: each drained event resolves with the live context — app
    focus refreshed per frame from egui `InputState.focused` (verified
    present in egui 0.36.2), workspace/pane focus from the model; the
    ring realizes `mark_unread` (upstream keeps unread on the focused
    path — the visible pane shows its own ring) and the smoke-visible log
    line now carries the full disposition.
  - Checks: `cargo test` **115 green** (8 new policy tests mirroring the
    upstream truth table), fmt + clippy clean; live smoke — headless pile
    window reports no focus, real OSC events resolved `external
    ring=true flash=true desktop=true` (banner armed), exit 0; oneshot
    unaffected.
  - Next: M3.3 agent hooks (Claude/Codex installers) on the M4 named-pipe
    IPC slice; `cmux notify` rides the same transport.

- **2026-10-18 — M3.2b: real Windows toasts landed (ADR-0011 amendment 2;
  owner picked the recommended AUMID approach).**
  - New `cmux-toast` crate is the Windows-specific boundary. API:
    `ToastDelivery::show(title, body, silent)` is fire-and-forget onto a
    lazily-spawned worker thread (MTA apartment once; UI thread pays only a
    channel send), plus pure helpers with tests (`xml::toast_xml`/
    `xml::escape_text`, `shortcut_path`). Failures log `cmux-toast: …` on
    stderr and never break the shell — the sidebar ring remains the
    notification of record, matching the upstream principle that toasts are
    one effect among seven (macOS upstream uses `UNUserNotificationCenter`
    with a permission prompt; Windows has none — the shortcut registration
    itself creates the Settings-listed app identity, verified).
  - AUMID registration = the documented Windows contract for unpackaged
    apps, implemented with **the `windows` crate 0.62.2 directly**
    (`tauri-winrt-notification` dropped): `IShellLinkW` → `IPersistFile`
    writes
    `%APPDATA%\Microsoft\Windows\Start Menu\Programs\cmux-shell.lnk`
    (target/icon = the running exe), then `SHGetPropertyStoreFromParsingName`
    (GPS_READWRITE) stamps `System.AppUserModel.ID` = `cmux.shell`.
    Idempotent: first toast job (re)checks the existing link's property and
    repairs on mismatch. Post path: generic `ToastGeneric` XML
    (`Data_Xml_Dom::XmlDocument`) → `ToastNotification` →
    `ToastNotificationManager::CreateToastNotifierWithId`.
  - Policy realization: `effects.desktop` gates the post; `effects.sound`
    maps to the toast audio (`sound=false` ⇒ `silent` attr; the external
    path plays the default notification sound). Toast title = OSC title, or
    `Workspace N` when OSC 9 carries none (OSC 9 has no title upstream
    either — attribution line shows `cmux` from the shortcut description).
    The two config keys (`notifications.suppressWhenAppFocused` /
    `soundWhenFocused`, both default `false` like upstream) now reach
    `policy::resolve` via `RuntimeSettings.notifications` — closing the
    M3.2a "until M3.2b" placeholder.
  - Fix recorded for the record: the first smoke crashed 0xC0000374 (heap
    corruption) — `PropVariantClear` frees `VT_LPWSTR` buffers with
    `CoTaskMemFree`, but the written variant's string was `Vec`-backed;
    reset the variant manually instead. Windows-API evidence was verified
    against registry sources (0.62.2 moved `PROPVARIANT` behind
    `Win32_System_Variant`, coclass GUID is `ShellLink` (no `CLSID_`
    prefix), `PKEY_AppUserModel_ID` lives under `EnhancedStorage` — hereby
    defined locally with the exact key GUID/pid).
  - Checks: `cargo test` **123 green** (+8 cmux-toast: XML escaping,
    template shape, silent mapping, path/identity), fmt + clippy clean.
    Machine smoke: live pane emitted `OSC 9` + `OSC 777;notify;Agent 42;…`,
    worker logged `ready (AUMID cmux.shell)` then two `posted` lines,
    **exit 0**; `Get-StartApps` independently lists `cmux-shell → cmux.shell`
    — the owner saw both desktop banners in real time. Owner session store
    untouched (temp `CMUX_SESSION_FILE`); temp scrubbed.
  - Known limits (documented): no toast activation/click handler (Windows
    requires a foreground activator — post-MVP), the dev-build lnk points
    at the debug exe until an installer slice, shortcut icon path can go
    stale if the crate moves (repair on AUMID mismatch only).

- **2026-10-18 — M3 started: M3.1 OSC notification ingress + workspace ring
  landed (ADR-0011; the ADR was accepted by the owner later that day).**
  - Subagent produced the upstream pipeline map (ingress, ring, delivery
    policy, turn detection, hook installers, history); the port-side
    feasibility check found `libghostty-vt` 0.2.2/`egui_tty` 0.2.0 expose no
    notification event, so the seam is ours: the PTY reader thread scans
    every chunk (`NotificationScanner`, pure lib module; events ride a
    second channel; bytes are untouched for the emulator).
  - Recognition mirrors upstream's byte-exact filter semantics: `OSC 9`,
    `OSC 777;notify;title;body`, BEL/ST terminators (including an ESC/`\`
    pair split across chunks), byte-exact selectors (`09;` rejected), bare
    ESC mid-payload abandons the candidate, 4096-byte ceiling.
  - Ring: per-workspace unread counters (`workspace_alerts`, aligned with
    `workspaces`), raising on events from any pane (hidden included);
    Select/Cycle clears the viewed slot (upstream workspace markRead rule);
    the sidebar row shows a warn-colored dot with the name shifted right.
    Not persisted; closed workspaces drop their slot atomically.
  - Checks: `cargo test` **107 green** (16 notifications scanner + 15
    pane-tree lib + 14 config + 22 shell bin + 11 workspaces + 27 layout
    + 2 ConPTY), fmt + clippy clean; machine
    smoke drove a live pane that printed `ESC ] 9;backend done` +
    `ESC ] 777;notify;Agent 42;…` — both logged, window closed cleanly,
    exit 0; restore + oneshot smokes unaffected; owner session store
    untouched (`CMUX_SESSION_FILE` temp).
  - Next slices: M3.2 toast (Windows AUMID decision pending) with the
    delivery-policy port; M3.3 agent hooks (Claude wrapper merge / Codex
    merge rules) on top of the first ADR-0005 named-pipe slice.

- **2026-10-18 — M2.5 themes subset landed (ADR-0008
  divergence #7).** Config gains `theme { scheme, background, cursor }`:
  - `scheme` `"dark"`/`"light"` maps onto the engine's built-in schemes.
    Evidence-first scoping: upstream drives terminal colors from Ghostty
    config files (`GhosttyConfig*`, `AppearanceMode{system,light,dark}`),
    and `egui_tty` 0.2.0 exposes only its two schemes (default fg/bg/palette
    are set behind `pub(crate)` in its `colors.rs`; no newer release
    exists), so the subset configures the scheme plus the port's chrome and
    Ghostty-config parsing stays post-MVP (feature row 23). The divergence
    is recorded in ADR-0008.
  - `background` (`#rrggbb`; default `#10141c`) fills the pane area and
    gutters; the sidebar derives one shade darker (`shade_darker`,
    ×0.88 per channel); `cursor` optionally overrides the terminal cursor
    while the program's own ANSI cursor color still wins. Per-field
    fallback: unknown scheme/hex logs and keeps defaults.
  - Template + config round-trip updated; the startup summary now logs
    `theme <scheme> <#hex>`.
  - Checks: `cargo test` 90 green (14 config + 27 layout + 21 shell bin +
    15 pane-tree + 11 workspaces + 2 ConPTY), fmt + clippy `-D warnings`
    clean; machine smokes: configured theme logs `theme light #010203` and
    exits 0 with a session save; invalid values (`solarized`, `#zzz`,
    `12g45z`) each log a fallback and exit 0; oneshot unaffected
    (`ONESHOT_FINAL_EXIT=0`); real-config run unaffected.
  - Small fix riding along: the sidebar's new-workspace button used the
    fullwidth ＋ (U+FF0B), absent from egui's bundled font (owner saw a
    tofu box) — now ASCII `+`.
  - Housekeeping: the plan file's non-ASCII characters had been corrupted
    by an ANSI-encoded intermediate write during this session; every
    occurrence was byte-mapped back to its original character (46× em-dash,
    15× arrow, 4× en-dash, plus ellipses/§/×/⌘/−).

- **2026-10-18 — M2.4b workspace tabs + vertical sidebar landed
  (ADR-0010).** Owner picked option A (workspace tabs over per-leaf
  panel strips). Upstream evidence: `newTab` renders "New Workspace",
  `nextSidebarTab`/`prevSidebarTab` "Next/Previous Workspace",
  `toggleSidebar` ⌘B "Toggle Left Sidebar", `closeTab` ⌘W "Close Tab" —
  a tab IS a workspace; the snapshot carries
  `tabManager.selectedWorkspaceIndex` over `workspaces[]`.
  - Port: `App` now holds `Workspaces { items: Vec<Workspace>,
    active }`, each workspace owning its own `PaneTree<Pane>` +
    focused pane; Fixed 216 px sidebar (+ new-workspace button, one
    clickable row per workspace with × close, active/hover fills from
    visuals) drawn after the panes so clicks never reach a terminal;
    row names derived live from the focused pane's shell title (OSC)
    with a positional `shell {n}` fallback (upstream `processTitle`
    rule); inactive workspaces' panes are polled every frame so shells
    keep running while hidden (hidden-pane PTY resize refresh is a
    recorded slice limitation). Close chain: pane → workspace → window
    (pane exit auto-close preserved per workspace).
  - Actions/keys (owner-accepted provisional set): `newTab`
    ctrl+shift+n, `closeTab` ctrl+shift+q (upstream ⌘W semantics:
    closes the whole active workspace), `selectWorkspaceByNumber`
    ctrl+1–9 bound row-wise from the digit key (a single-stroke rebind
    of the id is logged and rejected; ctrl+0 selects nothing),
    `nextSidebarTab` ctrl+shift+], `prevSidebarTab` ctrl+shift+[,
    `toggleSidebar` ctrl+shift+b. Sidebar clicks enqueue
    `SelectWorkspace(index)` / `CloseWorkspace { at }`, applied after
    painting to keep row indices stable.
  - Persistence: snapshot walks every workspace (ids `w1…`) + active
    index; restore builds each workspace independently — unbuildable
    workspaces are skipped with a log instead of discarding the
    session — and the restored active index is the saved index clamped
    to what survived. Fixed on the way: `cmux-workspaces` now uses
    serde_json's `float_roundtrip` feature (the default writer could
    decode `created_at` one ulp off, breaking byte-equal skip and the
    backup-fallback test on unlucky timestamps).
  - Checks: `cargo test` 82 green (11 config + 27 layout + 16 shell
    bin + 15 pane-tree + 11 workspaces + 2 ConPTY), fmt + clippy
    `-D warnings` clean; machine smokes: fresh-exit 1-workspace wire
    JSON; hand-built 3-workspace fixture restores as "3 workspace(s)
    with 4 pane(s) total" with the store SHA-256-identical after;
    auto-split → 3-pane save → restore round-trip; oneshot never
    restores/saves (`ONESHOT_FINAL_EXIT=0`).

- **2026-10-18 — M2.4a owner-verified.** Owner rubric passed on the owner
  machine: restart reproduces split layout/geometry/focus; panes closed by
  `exit` stay closed after restart; no regressions in cycling, close,
  exit-last-pane, or config application. ADR-0009 remains Proposed.

- **2026-10-18 — M2.4a session persistence landed (`cmux-workspaces`,
  ADR-0009).** Upstream evidence first: `AppSessionSnapshot{version,
  createdAt, windows[].tabManager}` with workspace layout encoded as
  `{"type":"pane"|"split"}` nodes (leaf = ordered `panelIds` +
  `selectedPanelId` — the tab model; split = `orientation`
  horizontal/vertical + `dividerPosition` + children in spatial order),
  `PanelType` raw `"terminal"`, and the repository semantics (sorted-key
  JSON, atomic writes with identical-content skip, primary + `-previous`
  under the Application Support equivalent, version mismatch/empty-window
  → unusable, unusable-primary backup recovery, richness panels-first).
  - Port: new crate `port/crates/cmux-workspaces` (serde/serde_json) with
    the subset wire shape, `SessionPaths` (`%APPDATA%\cmux\session.json`,
    `CMUX_SESSION_FILE` override), `load/save` mirroring the upstream
    store moves, and a restore-repair pass (dedup, empty-leaf collapse,
    selection/orphan/focus fallbacks, missing `terminal` panel
    synthesis, vanished-tree rebuild, divider clamp) driving the shell's
    startup restore. Pane leaves keep their full restored tab list
    (`panel_ids`, selected pane visible) so tabs round-trip losslessly
    ahead of the M2.4b tab-strip UI; exit-time save via eframe
    `App::on_exit` (upstream's 8 s autosave + history rotation deferred,
    recorded). Smokes skip restore+save (oneshot) or save
    (`CMUX_SHELL_RESTORE_SMOKE`) so machine runs never clobber a real
    session store.
  - Checks: `cargo test` 80 green (11 config + 27 layout + 14 shell bin +
    15 pane-tree + 11 workspaces + 2 ConPTY), clippy `-D warnings`, fmt
    clean; oneshot smoke unchanged; machine smokes: fresh exit saves the
    exact wire JSON; auto-split saves a split snapshot; restore smoke
    logs `restored 3 pane(s)` with the store hash-identical after;
    corrupt snapshot logs + starts fresh and self-heals on exit;
    restart + re-split restores and saves 4 uniquely-id'd panes.

- **2026-10-17 — M2.3 config file landed (`cmux-config`, ADR-0008).** Upstream
  evidence first: cmux's settings live in `~/.config/cmux/cmux.json`, JSONC
  tolerated on read, typed keys with decode-to-default semantics, typed
  `shortcuts.bindings` with stroke/chord/null forms and upstream action ids
  (`splitRight`, `splitDown`, `equalizeSplits`, `focusNextPane`,
  `focusPreviousPane`, `resizePane*` — `ShortcutAction.swift`),
  `globalFontMagnificationPercent` Int default 100.
  - New crate `port/crates/cmux-config`: JSONC read (own string-aware
    sanitizer stripping comments + trailing commas), serde model with
    per-field defaults and unknown-field tolerance, **save→load round-trip**
    (canonical pretty JSON; comment preservation + hot reload + chords are
    recorded divergences in ADR-0008), first-run template write, path
    `<home>/.config/cmux/cmux.json` + `CMUX_CONFIG_FILE` override, error
    enum with path-carrying messages.
  - Shell wiring: config at startup (missing → template + defaults; broken →
    log + defaults, never blocks the window); applied live — terminal font
    percent scales `TerminalStyle.font` (clamped 50–300%), `panes.gutter`
    replaces the `PANE_GUTTER` constant, `panes.resizeStep` feeds
    `PaneTree::resize_pane`, `terminal.shell` builds `PtySpec` (oneshot
    smoke keeps precedence), and `shortcuts.bindings` drives the binding
    table (stroke override / `null` unbind / unrecognized stroke falls back
    to default with a log). Keyboard `equalizeSplits` + `resizePane{L,R,U,D}`
    actions are now wired (they existed in the M2.1 layout model); defaults
    keep the owner-accepted provisional strokes, `resizePane*` unbound.
  - Checks: `cargo test` 64 green (11 config + 27 layout + 11 shell binding
    tests + 13 pane-tree + 2 ConPTY), clippy `-D warnings`, fmt, plus oneshot
    smokes: template-write/defaults, broken-config fallback (logged, exit 0),
    custom config (font 175%, gutter 25.5, 4 bindings = 5 defaults − 1
    unbind).

- **2026-10-17 — M2.2 follow-up: typing `exit` closes the pane.** Owner
  product decision (explicit, not silent): when a pane's shell exits —
  typing `exit`, `exit` in cmd/PowerShell, or a crash — that pane closes
  immediately, focus moves to a survivor, and closing the last pane closes
  the window, matching conventional terminal behavior (cmux/Windows
  Terminal/iTerm2).
  - Change: `poll_children` now treats a delivered watcher status as a
    close event (reason logged to stderr at pane granularity only — no
    terminal output is logged); the M1.1 per-pane notice row with
    **New shell**/**Close pane** and the `Action::Restart` affordance
    were removed as a consequence. A manual restart affordance returns
    with the config/menu milestone if wanted; keyboard pane close
    (Ctrl+Shift+W) is unchanged.
  - The M1.1 watcher-thread mechanism (no ConPTY EOF) is untouched; the
    **evidence class improves**: the one-shot smoke
    (`CMUX_SHELL_SPAWN_ONESHOT=1`) now runs fully machine-verified with
    no timers — *pane p1 closed — shell clean exit* logged, window
    closed by the auto-close path, process exit 0, no orphan cmd
    processes (5→5). Regular smoke (`AUTO_SPLIT_MS=1500`,
    `AUTO_EXIT_MS=5000`, real shells): 3 panes, no premature closes,
    timer close, exit 0.
  - Checks: `cargo test` 42 green (pane-tree tests unaffected),
    clippy `-D warnings`, fmt.

- **2026-10-17 — M2.2 follow-up: visible pane separation + focus ring.**
  Owner feedback: adjacent panes were hard to tell apart (two dark
  terminals touching). Renderer-side chrome added; the `cmux-layout`
  engine and `PaneTree` layout math are untouched, so all geometry tests
  keep passing unchanged:
  - `cmux_shell::panes::neighbor_sides/gutter_inset` (pure, over leaf
    frames only): a side that a sibling pane touches is inset by half of
    `PANE_GUTTER` (12 px total); flush sides stay full-bleed — a
    single-pane window keeps the M1 look, matching the terminal
    convention (Windows Terminal/iTerm2 gutter style). Unit-tested
    including the full-height-pane-beside-stacked-column overlap case and
    extent-equality vs overlap distinction (4 new tests → 13 in the
    crate, 42 workspace-wide).
  - `main.rs` render: each pane paints inside its inset rect; after all
    terminals, every pane rect gets `rect_stroke` (`StrokeKind::Inside`
    so the border never clips neighbor glyphs): unfocused 1 px from the
    theme's noninteractive widget stroke, focused 2 px from the theme's
    selection color, corner radius 4. The brighter focused ring also
    makes "who has the keyboard" visible — first pass at the cmux
    active-pane indicator using theme colors; real theme/config values
    are deferred to the config milestone.
  - Verification: `cargo test` (42) + clippy `-D warnings` + fmt green;
    auto-split smoke re-run (3 panes, ~8 s of painting) exit 0, no
    panics, no orphans. The visual result itself is for human
    inspection — machine checks cover the geometry tests and
    no-crash/no-orphan only.

- **2026-10-17 — M2.2 split panes wired into the shell.** The
  `cmux-layout` engine now drives a live UI. New pieces:
  - `port/crates/cmux-shell/src/panes.rs` (lib target `cmux_shell`,
    new `src/lib.rs`): pure `PaneTree<L: PaneLeaf>` over leaf payloads —
    `split_leaf`/`close_leaf` (sibling collapse), `layout` (divider-ratio
    rect division), `equalize`/`equalize_run_containing`/`resize_pane` by
    converting to `cmux_layout` snapshots from leaf frames and applying
    `DividerAdjustment`s back; `ordered_leaf_ids` DFS order. Direction
    semantics verified against `cmux-layout` (`Up/Left` insert first; Down:
    `0.5+120px/600px` row case) — 9 unit tests with dummy leaves, including
    the cross-orientation run-boundary case from the upstream suite.
  - `port/crates/cmux-shell/src/main.rs`: multi-pane App — leaves own
    `Terminal` + exit watcher; render via `ui.new_child(UiBuilder)`.
    max_rect(pane rect)); bindings parsed and consumed from
    `InputState` events before terminals see them (egui 0.36: events field
    is public, `Event: PartialEq`); focus model follows the keyboard by
    reading `Response::has_focus()` — egui_tty focuses its widget on
    click/drag itself (verified in egui_tty 0.2 source), cycle/new-pane/
    refocus use `Terminal::request_focus()` (one-shot pending flag).
    Provisional keys (until the keybinding/config milestone):
    Ctrl+Shift+D split below, Ctrl+Shift+R split right, Ctrl+Shift+W close
    focused pane (window if last), Ctrl+Shift+Tab previous, Ctrl+Tab next.
    Exit notice + **New shell**/**Close pane** exist per pane; closing the
    last pane closes the window.
  - Verification: 38 tests green workspace-wide (9 new), clippy
    `-D warnings` + fmt green; automated smoke
    `CMUX_SHELL_AUTO_SPLIT_MS=2000` + `CMUX_SHELL_AUTO_EXIT_MS=8000`
    printed *split-right queued*, *split-down queued*, *window close
    queued* and exited 0 with no orphan cmd/conhost processes (spawned
    panes go through the same `new_pane`/`split_leaf` path as bindings;
    spawn-failure eprintlns empty). **Machine-checked only: the queueing +
    lifecycle.** Not yet machine-checked (human rubric): on-screen layout,
    typing focus per pane, cycling, banner buttons, divisor drag (not
    implemented).
  - Known slice-stage limitations: dividers are plain ratios (no divider
    bars/gutters or drag yet); pane keyboard-resize bindings not wired
    (`PaneTree::resize_pane` exists and is tested); no tabs/workspaces or
    persistence yet; increment order swapped — config (M2.2→M2.3) lands
    after the visible split wiring because the config format needs
    upstream evidence reading first.
  - Housekeeping: `split_leaf`'s return contract is the NEW pane id; first
    smoke finding was a test-bookkeeping slip (interleaved pane/split id
    counters), not a product bug.

- **2026-10-17 — M2.1 `cmux-layout` geometry port landed.** First M2
  increment: `port/crates/cmux-layout` (std-only) — pane tree with pixel
  frames, `Orientation`, and the upstream `CmuxPanes/Geometry` ported
  one-for-one: global and split-run equalize plans, resize adjustments
  (legacy 0.1–0.9 clamp, enclosing-split candidate order), split-space
  verdicts including the 0.1–0.9 run-simulated clamping, spatial pane/panel
  ordering, `PaneCycleNavigator` focus cycling, and the
  `SplitDirection`/`ResizeDirection` values. Spec files read from
  `cmux-main/Packages/macOS/CmuxPanes` (Geometry, Values, PaneCycleNavigator
  read from source, not inferred from tests; bonsplit skip rationale:
  ADR-0007). Tests are the upstream Swift suites ported 1:1 — **27/27
  pass** — and caught a real porting bug first hand (splitting-pane
  prospect span computed as `bool*2+1` = 3 instead of 2). Recorded
  adaptations: `String` split ids (no UUID parsing → no
  `hadInvalidSplitIds`), `Orientation` enum, no tab metadata in the tree.
  License **GPL-3.0-or-later** (derivative of CmuxPanes), flagged in
  ADR-0007. Whole-workspace `cargo test` (29) and `clippy -D warnings`
  green.
- **2026-10-17 — M0 green.** Toolchain installed (owner-approved): rustc
  1.99.1 stable-msvc (minimal profile + rustfmt/clippy), Zig 0.15.2 at
  `C:\Users\noah\tools` (C-drive on purpose: uucode drive-letter bug, see
  `building-windows.md`). Workspace: `cmux-pty-port` (+ `cmux-testsupport`),
  `cargo build/test/fmt/clippy -D warnings` all green. ConPTY truths moved
  from hypothesis to verified fact in ADR-0003 (no EOF, writer lifetime,
  0.9.0 wait-hang regression → pin 0.8.1, injected mode-set/DSR/OSC noise).
- **2026-10-17 — M1 accepted (owner manual run).** Owner ran the slice
  interactively and reports typing/commands working as expected. The rubric
  below stays as the standing re-check list for future changes.
- **2026-10-17 — M1.1 pane-exit lifecycle (user-reported gap).** Owner observed
  `exit` left a dead silent pane: nothing observed the child (ConPTY gives no
  EOF, so the reader loop alone cannot detect exit). Fix:
  `PtySession::into_parts` (cmux-pty-port) splits reader/writer/master plus
  the child handle; a watcher thread blocks on `child.wait()` and reports the
  status over a channel; the pane polls it per frame and then shows an exit
  notice with **New shell** (fresh session) / **Close** actions.
  Verified: automated one-shot smoke (`CMUX_SHELL_SPAWN_ONESHOT=1` spawns
  `cmd /c exit`, `CMUX_SHELL_AUTO_EXIT_MS=5000`) — child exit surfaced,
  notice path rendered without panic, window exit code 0, no orphaned
  processes. Records for M2 UI code: egui 0.36 removed `TopBottomPanel` and
  the free `egui::horizontal` (use the unified `Panel` API /
  `ui.horizontal`); portable-pty 0.8.1 `Child::wait` returns `io::Result`
  (0.9.0 switched to anyhow) — normalized in the watcher.
- **2026-10-17 — M1 machine-verified.** `cmux-shell` (eframe 0.36 + egui_tty
  0.2 (libghostty-vt 0.2.2, Ghostty VT built from source by zig 0.15.2
  without workarounds) + our ConPTY session): builds clean; automated smoke
  (`CMUX_SHELL_AUTO_EXIT_MS=6000`) ran a real `powershell.exe -NoLogo`
  through ConPTY inside the window and exited 0; no orphaned processes
  after exit. `egui` needed as a direct dependency (0.36) to use the widget
  in our own crate — noted here for reproducing.
- **M1 human rubric (owner run done 2026-10-17):** interactive
  typing/printing, arrows & control keys, full-screen program, window-resize
  reflow, Ctrl+C, CJK IME, HiDPI, clipboard round-trip. Run with `cargo run`
  after `. .\scripts\env.ps1` from `port/`.

## 6. Testing strategy

See `docs/testing/test-strategy.md`. Summary: unit-per-crate; PTY stream
golden captures; integration tests with real pwsh/cmd incl. cleanup
verification; the upstream Python harness pattern (`tests_v2/cmux.py`
protocol) replicated against the port's IPC for cross-checking behavior with
the reference; regression commits before fixes (upstream convention); CI on
`windows-latest` plus optional Linux lane for cross-platform sanity.
