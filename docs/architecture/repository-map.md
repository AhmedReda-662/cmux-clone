# Repository map — cmux (reference) and the Windows port

Verified 2026-10-17 from the source snapshot at `cmux-main/` (upstream
manaflow-ai/cmux, a macOS-only Ghostty-based terminal). Paths below are
relative to that snapshot unless stated. Findings gathered by five scoped
exploration passes plus targeted reads; classifications marked *[in] =
inference, un = unverified locally (source or dependency not present in the
snapshot)*.

## 0. Snapshot facts

- The workspace root is not a git repository; `cmux-main/` is a plain copy
  with **no `.git`** and **no initialized submodules**: `ghostty/` (Ghostty
  fork, engine source), `homebrew-cmux/`, `vendor/bonsplit/` (split-pane
  Swift layout package used by the app) are **empty stubs**.
  `vendor/WireGuardKit/`, `vendor/stack-auth-swift-sdk-prerelease/`, and
  `cmux-tui/vendor/crossterm/` contain real source.
- No built artifacts exist (no `GhosttyKit.xcframework`, no Rust target dirs).
- License: repo `LICENSE` = GPL-3.0-or-later (app/CLI/most directories); six
  server directories are BUSL-1.1 (see §6). Note: the `cmux-tui` workspace
  Cargo metadata declares `license = "MIT"` — a **discrepancy against
  LICENSE** to escalate before reusing crate code (§6).

## 1. Application entry points (macOS)

- `Sources/CmuxMain.swift` — `@main`; ordered startup (crash policy, fd-limit
  raise, worker-process self-re-exec, resume-approval store) then app main.
- `Sources/cmuxApp.swift` — SwiftUI composition root (owns defaults/runtimes;
  `TabManager`, `TerminalNotificationStore`, …); AppKit via
  `AppDelegate` (`Sources/AppDelegate.swift`, ~21k lines; socket listener,
  workspace lookup, shortcut routing).
- `CLI/cmux.swift` — the CLI (43k lines, hand-rolled parser; contract in
  `docs/cli-contract.md`, ~250 commands).

## 2. Module map

| Area | Where | Notes for the port |
| --- | --- | --- |
| Window/UI framework | `Sources/` (SwiftUI + AppKit; `GhosttyNSView`, `GhosttyMetalLayer`), `Packages/macOS/CmuxWindowing` | macOS-only by construction; Rust GUI replaces all of it |
| Workspaces model | `Packages/macOS/CmuxWorkspaces` (`WorkspacesModel`, `WorkspaceGroup*`), `Sources/Workspace.swift` (per-workspace composition root) | model classes Swift; ordering/grouping rules portable |
| Panes/splits | `Packages/macOS/CmuxPanes` (`PaneTreeModel`, `SplitLayoutModel`, `Geometry/*` — pure divider-equalize/spatial-order math), authoritative tree in `BonsplitController` (vendor/bonsplit, **un**) | geometry math is the best direct-port candidate; bonsplit semantics must be re-derived *[un]* |
| Terminal engine | `GhosttyKit.xcframework` (prebuilt libghostty; fetched checksum-pinned or built via Zig) + app bridge `cmux-Bridging-Header.h`/root `ghostty.h` | full engine incl. Metal renderer + internal shell spawn; macOS-only as packaged |
| Terminal surfaces | `Packages/macOS/CmuxTerminal` (`TerminalSurface*`, spawn policy, teardown coordinator, manual-I/O mode `io_mode` + `ghostty_surface_process_output`/`io_write_cb`), `Packages/Shared/CmuxTerminalCore` (`GhosttyRuntimeCInterop`) | manual-I/O mode already decouples PTY from emulation conceptually |
| Rust engine binding | `cmux-tui/crates/ghostty-vt-sys` (bindgen over `ghostty/include/ghostty/vt.h`; builds `libghostty-vt.a` with Zig 0.16.0; **Windows MSVC/GNU targets explicitly gated**), `ghostty-vt` safe wrapper | most reusable component; Windows use requires fetching ghostty source *[un until built]* |
| Rust PTY | `cmux-tui/crates/cmux-pty` (re-exports portable-pty traits; hardened Unix backend in `src/macos.rs`; **no cfg(windows)** — relies on portable-pty native = ConPTY) | PTY seam level: fine; ownership/cleanup extras need Windows backend |
| Rust app core | `cmux-tui/crates/cmux-tui-core` (surface/terminal-host runtime, hooks into `Terminal` callbacks; renders via ratatui+crossterm), `cmux-terminal-client` (small C ABI snapshot/output stream) | reference for event wiring; front end is not GPU-grade |
| Remote/SSH | `Packages/macOS/CmuxRemoteSession` (OpenSSH ControlMaster lifecycle, `RemoteSessionCoordinator`, tmux `-CC` layout mirror), `CmuxRemoteWorkspace` (relay auth policy, PTY bridge), `CmuxRemoteDaemon`; Rust `cmux-tui/crates/cmux-remote` (ssh bootstrap/staging, providers ssh/iroh/websocket/relay, **`unix_socket.rs` hard Unix-gated**) | ControlMaster and `tmux -CC` have no Windows equivalent — reconnection strategy is an open design item |
| Browser (native pane) | `Packages/macOS/CmuxBrowser` (`CmuxWebView` WKWebView; automation = WebKit JS injection + private selectors; `Automation/`, `Repl/`), `Sources/Panels/BrowserPanel*`; importers for Chrome/Firefox/Arc/Safari | automation API shape (agent-browser port, `docs/agent-browser-port-spec.md`, 125 actions) portable; substrate must become WebView2 |
| Browser (CDP) | `cmux-tui/crates/cmux-tui-cdp` (minimal CDP client), `cmux-tui/docs/browser-panes.md` | CDP path is the portable one on Windows (WebView2 speaks CDP) |
| Control IPC | `Packages/macOS/CmuxControlSocket` — **AF_UNIX SOCK_STREAM**; v2 JSON-lines (`{"id","method","params"}`), ~50 method families across domain coordinators; auth modes `off/cmuxOnly/automation/password/allowAll`; peer identity via `LOCAL_PEERTOKEN`/`PEERPID`/`PEERCRED` + Keychain capability proof | protocol/dispatch portable; transport+identity must become named pipes + DACL + client-PID ancestry |
| CLI/auth discovery | `CLI/CLISocketPathResolver.swift` (`--socket` > `CMUX_SOCKET_PATH` > candidates incl. `/tmp/cmux*.sock`), password from Keychain/`CMUX_SOCKET_PASSWORD` | Windows: per-user named pipe namespace + Credential Manager/DPAPI |
| Agent detection | Hooks (per-agent catalogs `CLI/CMUXCLI+AgentHookCatalog.swift`, `cmux hooks setup`, spool `CmuxAgentLaunch/AgentHookSpool*`) → `CmuxAgentJournal` reducer (lifecycle states) → rings; screen-level detection `cmux-tui/bindings/examples/rust-agent-screen-detection` (herdr-derived manifests, **Apache-2.0** with pinned revisions — clean reuse) | Rust detector is cross-platform; hook config-writing is portable |
| Notifications | OSC 9/99/777 → `GhosttyDesktopNotificationIngress` (bounded queue) → `TerminalNotificationStore` (unread/ring indexes) → `UNUserNotificationCenter` | queue/index logic portable; native delivery landed as `cmux-toast` (direct `windows` 0.62.2 WinRT: AUMID Start Menu shortcut + `ToastNotificationManager`; `tauri-winrt-notification` not needed) |
| Config | `Packages/macOS/CmuxSettings` (`JSONConfigStore` actor, JSONC-tolerant, comment-preserving path edits, atomic publish, semantic validation) at `~/.config/cmux/cmux.json`; Ghostty config coupling (themes/fonts) via `CmuxTerminalCore/GhosttyConfigDiscovery*` + live reload | store design portable; decide Ghostty-config compatibility vs native format |
| Session persistence | `Packages/macOS/CmuxWorkspaces/Session/` (`SessionSnapshotRepository`: versioned JSON snapshots in Application Support, primary+backup, rich-ness grading); restore via `session.import/export` RPC | JSON format portable; contents (PTY handles, agent liveness) need Windows re-implementation |
| Git/PR/ports metadata | `Packages/macOS/CmuxSidebarGit`, sidebar metadata providers, listening-port detection (upstream README feature) | read detail only at Moo+; classification done from feature docs *[in]* |
| iOS/mobile (`ios/`, `Packages/iOS`, `TunnelExtension/`) | out of scope for the port (keep out of BUSL dirs; unrelated) |  |

## 3. Tests and CI (reference only)

- Swift: `cmuxTests/` (1288 files, ~346 classes), `cmuxUITests/` (70),
  `cmuxCLITests/` (39). macOS/Xcode required; UI tests need a GUI session.
- Python: `tests/` (440 files) + `tests_v2/` (144) drive the app's control
  socket for end-to-end behavior; `tests/test-execution.toml` maps lanes —
  ~267 of 430 v1 tests are Linux-runnable. The harness pattern (fresh
  workspace per test, `read_screen`/`send`, retries with app relaunch) is the
  model for port E2E.
- Tooling runnable here: `python scripts/verify-local.py` (16 diff-scoped
  static checks), biome for JS/TS. Rust: `cargo test` in `cmux-tui/` needs Zig
  0.16.0 + the missing ghostty submodule.
- CI: ~140 workflows (macOS app lanes, Rust TUI hosted lanes, web, iOS,
  nightlies, `build-ghosttykit.yml` builds the pinned framework).

## 4. Naming traps

- `class TerminalController` (`Sources/TerminalController.swift`) is the
  **Unix-socket automation controller**, not a terminal view.
- `cmux-pty/src/macos.rs` is a **generic Unix** (`cfg(unix)`) backend, not
  macOS-only.
- `Sources/` has no `Shared`/`macOS` split; shared/`macOS` packages live under
  `Packages/`.

## 5. What the port reuses, adapts, or replaces

- **Reuse (Rust, subject to license resolution §6):** `ghostty-vt`/`ghostty-vt-sys`
  binding approach; `cmux-pty` trait surface; `rust-agent-screen-detection`
  (Apache-2.0); CDP client concepts from `cmux-tui-cdp`; protocol/JSON-lines
  v2 design.
- **Adapt (concept→Rust):** split-geometry algorithms (`CmuxPanes/Geometry`),
  notification store/ring models, JSONC config store semantics, CLI contract
  (`docs/cli-contract.md`), session-snapshot schema, test-harness pattern.
- **Replace:** AppKit/SwiftUI/NSWindow (→ Rust GUI), WKWebView+JS injection
  (→ WebView2 + CDP), UNUserNotificationCenter (→ WinRT), Keychain (→
  Credential Manager/DPAPI), AF_UNIX transports (→ named pipes), posix_spawn
  helpers (→ CreateProcess + Job Objects), tmux `-CC` mirror (→ to be
  classified), `/etc/shells`+`$SHELL` (→ pwsh/cmd/WSL discovery), Ghostty full
  engine + Metal (→ ADR-0002 engine + Rust renderer).
- **Config (landed):** `port/crates/cmux-config` mirrors upstream's
  `JSONConfigStore`: JSONC tolerated on read, typed fields with defaults,
  `<home>/.config/cmux/cmux.json` (ADR-0008), incl. the `theme` subset
  (scheme + chrome colors; ghostty-config parsing stays post-MVP) and the
  `notifications` subset (`suppressWhenAppFocused`/`soundWhenFocused`)
  and the `commands` entries (custom commands; terminal flavor runs,
  workspace flavor parses for upstream-file compatibility, ADR-0008
  Amendment 1).
- **Session store (landed M2.4a):** `port/crates/cmux-workspaces` mirrors
  upstream's `SessionSnapshotRepository` + the `SessionPersistence`
  snapshot graph subset: `%APPDATA%\cmux\session.json` (+ `-previous`),
  same `{type:"pane"/"split"}` layout wire format, version/`hasWindows`
  gates, unusable-primary backup recovery, restore-time repair; exit-time
  save (autosave deferred). ADR-0009.
- **Workspace tabs (landed M2.4b):** `cmux-shell`'s `Workspaces` holds one
  pane tree + focused pane per workspace tab with a fixed sidebar
  (216 px when it landed; the design width 256 — node 1:399 — since
  design-fidelity round 1, 2026-10-26) (＋/×/click rows, live
  shell-title names); inactive panes stay alive via
  per-frame `poll()`; every workspace + the active index persist
  (ADR-0010). Round 9 (2026-10-27) deepened the strip: the rows +
  services segment scroll as one column (`cmux_shell::sidebar` holds
  the pure scroll/planner/rename math), wheel over the content region
  only, edge fades, reveal-on-select, drag-reorder with the planner's
  no-op suppression, drag autoscroll, double-click inline rename with
  upstream's `titleToCommit` commit policy, and `customTitle` rides
  the session snapshot round trip (ADR-0010 update;
  test_sidebar_depth_r9.py 35/35).
- **Notifications (landed M3.1 + M3.2a + M3.2b):** the PTY reader thread scans its
  chunk stream with `cmux-shell`'s `notifications::NotificationScanner`
  (wire semantics from upstream's `RemoteTmuxNotificationOSCFilter`);
  events run through `policy::resolve` (upstream's
  `TerminalNotificationDeliveryDecision` — focus gates, effects) and raise
  per-workspace unread rings in the sidebar (ADR-0011). `effects.desktop`
  posts a real WinRT toast through the `cmux-toast` crate
  (`windows` 0.62.2: per-user Start Menu shortcut registering the
  `cmux.shell` AUMID + `ToastNotificationManager`; worker thread, best
  effort, crash observed/free-freed fixed — see ADR-0011 amendment 2).
  Hook events land on the same pipeline since M3.3 slice A (below).
- **Hook ingress (landed M3.3 slice A + M3.3b):** `port/crates/cmux-ipc` is
  the control-pipe server/client (JSON-lines v2, `ping` +
  `agent.hook.enqueue`, DACL + ancestry peer check, ADR-0005 Amendment 1);
  `port/crates/cmux-hooks` is the config engine (codex:
  hooks.json merge/prune, canonical trust hashes, `config.toml`
  marker-block editor; claude: the settings-document generator with
  upstream's exact event table in exec form, ADR-0011 Amendment 4);
  `port/crates/cmux-cli` (binary `cmux`) is the CLI
  (`ping` / `hooks enqueue` / `hooks feed --source <agent>` /
  `hooks codex install|uninstall` / `hooks claude inject-settings` /
  `hooks claude launch` — the PATH-shim target with pass-through +
  exit-code semantics + fail-open claude verb stubs — always-exit-0 on
  the hook paths; plus the M6 `browser` wrapper over the 37 landed browser.* wire verbs per upstream grammar - ADR-0006 Amendment 6; plus the M6 `command` wrappers (`cmux command list|run`) over the config-file custom commands - ADR-0008 Amendment 1; plus the design-lane-5 `cmux agent` wrapper over `agent.open_split` - port extension, upstream has no agent verb).
  `cmux-shell` starts the server, drains admitted events into the shared
  `policy::resolve` pipeline (journal-route subcommands `feed`/
  `pre-tool-use` log without ring/toast), injects the pane identity env
  (`CMUX_SURFACE_ID`/`CMUX_WORKSPACE_ID`/`CMUX_SOCKET_PATH`,
  `CMUX_CLAUDE_HOOKS_DISABLED` forwarded when present) and routes the
  pane PATH explicitly (shim slot prepended when enabled), installs the
  typed-claude shim (`%LOCALAPPDATA%\cmux\claude-settings\shims`), and,
  when a pane's program is the claude CLI by explicit path, spawns it
  with `--settings <doc>` from
  `%LOCALAPPDATA%\cmux\claude-settings` (ADR-0011 Amendments 4-5). The
  automation surface is live (ADR-0005 Amendments 2-3): `ping`/`system.ping`/
  `system.capabilities` answered in the server, the app-plane subset
  (`system.identify`, `workspace.list/current/select/create`, `surface.list`,
  `pane.list`, `surface.send_text`, `surface.read_text`) executes on the shell
  frame loop through the request plane, pane/workspace ids are process-globally
  minted (`p1`/`w1` first, seeded on restore), and the CLI exposes the verbs
  plus the global `--socket`/`--password` flags. Access modes moved to
  `CMUX_SOCKET_MODE` (`cmuxOnly` = DACL + ancestry, `automation` = DACL only,
  `password` = DACL + the per-connection `auth.login`/`auth <pw>` handshake
  with SHA-256 fingerprint rotation checks); the credential lives in
  `cmux-ipc/password.rs` (explicit > `CMUX_SOCKET_PASSWORD` env >
  `%APPDATA%\cmux\socket-control-password` DPAPI blob; upstream texts kept
  verbatim, incl. the CLI's `Unknown command 'auth'` tolerance). The
  connection front is a pool since M4.4 (ADR-0005 Amendment 4): the pipe
  name holds 8 concurrent instances, one serve thread each — a parked or
  silent client can no longer starve other connections (per-connection
  auth/rotation semantics unchanged; the name claim precedes the pool
  spawn, keeping the second-instance `IPC disabled` path). Remaining v2
  surface (more coordinators, `surface.type`/selection verbs,
  browser-lane methods) is Post-MVP; saturation answers the OS-level
  `ERROR_PIPE_BUSY` + client retry instead of an in-band `overloaded`
  reply (recorded divergence).

- **Terminal UX (landed M4.3):** `cmux-shell/src/fonts.rs` installs the
  machine's Windows faces into egui at eframe creation (Cascadia → Consolas
  grid primary + YaHei/JhengHei/MS Gothic/Malgun CJK coverage chain, silent
  default degrade, startup `fonts:` log; ADR-0012), and `shell_spec` no
  longer forces `TERM`/`COLORTERM` (Windows-Terminal-parity Ctrl+C
  semantics under ConPTY; ADR-0013). The live-input driver lane
  `port/harness/test_terminal_ux_m43.py` (stdlib ctypes: SendInput
  injection with per-frame pacing, DPI-scaled coordinates, US-layout pin,
  clipboard poll) runs 19 live checks over rows 5/6/7 end to end;
  relocated baseline smokes live beside it
  (`smoke-m34-turn.ps1`, `smoke-cli.ps1`).

- **Browser panes (landed M5.1, focus unification in M5.2):**
  `port/crates/cmux-webview2` wraps WebView2 (`webview2-com` 0.39; library
  crate) around the managed `BrowserHost` the shell's frame loop owns —
  async env→controller→wire lifecycle, per-frame `tick` applying
  bounds/visibility/focus/window-pos with 20 ms throttled URL/title
  polling, one user-data folder per surface under
  `%LOCALAPPDATA%\cmux\webview2\<pane-id>`, and an `AccelRouter` that
  captures accelerator keys while a webview holds native focus — since
  M5.2 the capture is a real chord only while Ctrl is actually held
  (`routes_as_chord` + `GetKeyState(VK_CONTROL)` at the COM callback), and
  the router also queues per-surface `GotFocus` arrivals (`BrowserSpec`
  carries `surface_id`; unit tests cover dip-rect→device-pixel rounding,
  the chord combos, and the router's match/drain order). Since the
  round-7 palette rider the spec also carries `default_background` -
  the shell's resolved viewport fill (the design's `#0c0e14` token,
  `theme.background` override included) - threaded through the
  single `BrowserSpec::new` call site: controllers request through
  `ICoreWebView2Environment10` options carrying that color (the
  documented early-init path for the dark-mode theme-derived base
  paint; upstream issues 4389/5752) with a plain-call fallback and a
  wire-time `ICoreWebView2Controller2::SetDefaultBackgroundColor`
  backstop. Live-verified on Evergreen 156.0.4314.42: pages with no
  defined background paint the token; the bare `about:blank` leaf
  keeps Chromium's theme gray (Edge special-cases the blank
  document) - and since owner round 8 NEW leaves no longer start
  there: `open_split` defaults to the token-painted home data URL
  (`browser_home_url`), leaving `about:blank` to restored legacy
  leaves only.
  `cmux-shell` holds browser leaves as `PaneBody::Browser(PaneBrowser)`;
  `browser.*` IPC verbs and identify/surface facts ride the existing
  request plane; model focus reconciles through the unit-tested
  `focus_reconcile_target` (explicit click claims first, then egui focus
  transitions), the shell drains the GotFocus queue for active-workspace
  leaves, `render_pane` surrenders standing egui focus while a browser
  leaf owns the model, and drained chords re-enter as app actions.
  **M5.2 CDP automation (ADR-0006 Amendment 3)**: the crate grew the
  automation plumbing — `call_cdp` (issues
  `CallDevToolsProtocolMethod`, tracks host-side round trips in
  `CdpTracker`), a pump-thread completion outbox drained by
  `take_cdp_completions`, and `FrameWakeFn` to wake the frame loop;
  the shell runs the automation state machine: `serve_ipc_calls`
  validates + shapes + issues phase 1 and parks `PendingCdp`, and
  `advance_cdp` escalates click/dblclick (fresh-center evaluate →
  `Input.dispatchMouseEvent` mousePressed → mouseReleased, clickCount
  1 or 2) and type (focus evaluate → `Input.insertText`,
  whole-string insert a recorded divergence) before shaping the reply;
  `browser.snapshot` lives in one `Runtime.evaluate` walker (original
  port code, upstream parity semantic: text lines + snapshot-local
  `@eN` refs with selectors, any navigation clears the pane's ref
  table; selectors embed as serde_json JSON literals, injection-safe);
  slow COM ticks (>5 ms, rate-limited) and slow CDP completions (>100
  ms) log permanently for the blocking-COM question (ADR-0006
  Amendments 1-3; live smoke
  `port/harness/test_browser_m51.py`, 87 checks incl. the restart leg
  and the automation section on the second instance).
  **M5.3 observation + form/scroll family (ADR-0006 Amendment 4)**:
  APP_METHODS grew 20 → 42 with `get.*`/`is.*`/`fill`/`focus`/`check`/
  `uncheck`/`select`/`hover`/`scroll`/`scroll_into_view`/`screenshot`/
  `focus_webview`/`is_webview_focused`; every element verb takes
  `ref` XOR `selector` (the upstream CLI grammar) and its reply echoes
  the consumed flavor; the shell executes the one-evaluate `ElementJs`
  verbs on the M5.2 deferred plane, hover adds the `AfterHoverCenter`
  step (fresh center + one `Input.dispatchMouseEvent` `mouseMoved`), and
  `browser.screenshot` decodes `Page.captureScreenshot` (base64,
  cmux-shell only) into the `%TEMP%` staging lane, replying with the
  written path; `get.title` rides the sync lane and the focus verbs are
  model-plane. Also folded in: `browser.eval` accepts upstream's
  `script` field name as a fallback for the canonical `expression`.
  **M5.4 key synthesis + waits (ADR-0006 Amendment 5)**: APP_METHODS grew 42 to 46 with `browser.press`/`keydown`/`keyup` and `browser.wait`. The key family resolves a caller token (`Enter`, `a`, `Control+a`, `+`, aliases like `esc`/`return`/`down`) through the port's Playwright-style `key_table()` (~82 rows: named keys and the four modifier keys, letters/digits with US shift partners, 11 US punctuation rows with VKs 0xBA-0xDE) into `Input.dispatchKeyEvent` events keyed to the focused element: printing keys' down events are `keyDown` carrying the shift-aware char (Ctrl/Alt/Meta suppress it), non-printing are `rawKeyDown`, up halves are `keyUp`, and press = the down event then the prepared up through the `CdpStep::AfterKeyDown` step. `browser.wait` polls ONE combined evaluate (slots in documented order, string predicates JSON-encoded, the function predicate verbatim at eval's trust level) on a completion-driven re-arm loop until the deadline; the pending sweep anchor became `last_activity` (refreshed per re-arm, so a live wait is never swept), the wait's per-call reply budget extends to deadline + 8 s headroom (upstream's CLI socket-headroom formula) while every other call keeps 400 ms, satisfied replies carry `waited_ms`, and deadline hits surface the structured `timeout` problem instead of the generic envelope. The spec's P0 automation list is closed; the M5.4 battery lessons (probe-vs-smoke drift, survivor hygiene) are recorded in test-strategy section 12.
  **M6 slice 1 custom commands (ADR-0008 Amendment 1)**: APP_METHODS
  grew 46 to 48 with `command.list` (config-order rows: name,
  description, confirm, kind = terminal|workspace, and the
  sanitized `command` text for terminal rows) and `command.run`
  (`{name, surface_id?, ack}`; `ack` is bool-only). The run checks
  the definition from a call-time, side-effect-free config read
  BEFORE resolving the surface: unknown name -> not_found,
  workspace flavor or unacknowledged confirm -> invalid_state
  (upstream confirms via a dialog; the port's ack replaces it and
  the CLI's `--yes` maps to ack:true), then the send_text surface
  rules and terminal.send(command + "\n") - the invisible-charset
  sanitize runs in the app so the trust boundary stays where
  upstream keeps it. Workspace-flavor entries parse for
  upstream-file compatibility and answer the recorded residual.
  **M6 SSH verb (ADR-0014 amendment 1)**: APP_METHODS grew 48 to 49
  with `workspace.ssh.open` (`destination` required, `ssh_options`
  array of strings, `port` 1-65535, `identity_file`/`title`/
  `initial_command`, `focus` bool defaulting true) - the app spawns
  a new workspace whose single pane child is `ssh.exe` with OpenSSH
  argv ordering (options before the destination, remote command
  after) and the upstream runSSHTui handoff shape. The CLI verb
  `cmux ssh` mirrors upstream's parser subset with verbatim refusal
  texts and encodes a pure `-t`/`-T` run as the effective
  `RequestTTY` option; the app-known pane title (destination or
  `--name`) wins over child OSC titles (Windows ssh.exe self-titles
  with its image path), and `--no-focus` leaves the active
  workspace unchanged.
- **Agent activity stream pane (design lane 5, landed 2026-10-26):**
  APP_METHODS grew 49 to 50 with `agent.open_split` (reply = the
  browser open-split shape + `"type": "agent"`); the CLI gained
  `cmux agent [--surface <spec>] open [--json]` (upstream has no agent
  verb; other subcommands refuse with the reference-doc pointer).
  `cmux-shell` gained `PaneBody::Agent`: a read-only pane with no child
  process, a click interceptor + a11y label, bottom-anchored painting,
  and a pure `agent_cards` card store (24 cards per workspace;
  workspace-close clears). Cards come only from REAL ingest paths
  (OSC 9/777 notifications, turn-complete deliveries, validated hook
  events with journal routes included - card-only, never ring/toast);
  pending boundaries render as the accent-ringed active card with the
  frozen terminal tail. Persistence stays truthful: agent leaves save
  `panel_type: "agent"` and restore rebuilds them with an empty
  stream (cards are not persisted). Visuals and probe evidence:
  `docs/research/figma-design-reference.md` sec. 13 +
  `port/harness/test_agent_activity_lane5.py` (36 checks).
- **Design-fidelity round 1 (landed 2026-10-26):** the chrome follows
  ONE theme ladder — every chip/pill/strip/row fill paints through
  `ThemeRuntime` (`background`/`sidebar_background`/
  `viewport_background`/`chip_background`; the new `chip_background`
  is the `#1d1f26` design token by default and derives
  `shade_lighter(sidebar)` when a config `theme.background` is set,
  completing ADR-0008 divergence 7's precedence coherently;
  `#191b22/0.88 → #1c1f27` vs the flat chip token is a recorded
  ±1/channel approximation). The aside paints the design width 256
  (node 1:399; it painted 216 before). Windows adaptations: a
  `friendly_osc_title` fold (absolute drive/UNC `.exe` image path →
  stem) keeps PowerShell 5.1's self-title human in the
  breadcrumb/sidebar/marker consumers (`title_override` still wins —
  the M6 ssh rule), and `draw_status_bar` budgets its clusters
  (right cluster measured first, CPU/MEM/agent telemetry reserved,
  pipe-leaf and ws-label folds; the marker fold + overflow ellipsis
  retired with the markers in owner round 8) so long child chatter
  cannot push the design telemetry
  off the strip. Pane-interior card faces stay the raw design
  tokens (content, not chrome). Evidence + live verification:
  figma-design-reference.md sec. 14; the lane/m43/m51 probe
  geometry re-anchored to 256 and green; the owner's legacy `theme`
  block in their personal `cmux.json` stays theirs (dropping it is
  an owner decision).
- **Design-fidelity round 2 (landed 2026-10-26):** the sub-header
  strip now starts at the aside's right edge (`sidebar_rect.right()`;
  node 1:399 keeps the aside full-height under the 32px title row)
  so the aside's `+` (new workspace) row is visible — the strip had
  painted over it from the window edge. Its click queues
  `Action::NewWorkspace`, live-verified: one click → IPC
  `workspace.list` 1 → 2 workspaces. The workspace-row close glyph
  is two hand-drawn `painter.line_segment` strokes (the old source
  literal carried mojibake bytes — byte audit ground truth). The
  owner's legacy `theme` block was dropped from their `cmux.json`
  with explicit authorization: the unconfigured ladder is the exact
  Figma palette (ladder semantics unchanged). Evidence:
  figma-design-reference.md sec. 15; full battery + the plus-click
  probe green on the round-2 binary.
- **Owner-directed round 3 (2026-10-26):** the status bar's CPU/MEM +
  agent telemetry run paints a smaller 10px font (pane markers stay
  12px; measured with the painting font, so the reserve-then-fold
  budget keeps the telemetry collision-free), and the aside's bottom
  version strip (design node 1:485) is no longer drawn — both owner
  decisions diverging from the mock; the released 38pt budget lets
  the workspace and RUNTIME SERVICES rows reach 6pt above the aside
  bottom. Evidence: figma-design-reference.md sec. 16; full battery
  + pixel checks green on the round-3 binary.
- **Owner-directed round 4 (2026-10-26):** the design lane 3 sub-header
  band (node 1:5) is no longer drawn — `main_rect` starts at the
  title row's bottom, the palette overlay anchors at title+8, and the
  dead private code around the band (git sampler plumbing, keycap
  helpers, tray glyphs, agent-chip violet pair) left with the
  surface; `git_state` and the palette survive; `equalizeSplits`
  stays config-bindable (disclosed: no click surface left).
  test_subheader_lane3.py is the band-absence probe now (15+22,
  verified-pin recipe ported from 2b), lane-5 re-anchored to the
  risen panes (36/36 back). Evidence: figma-design-reference.md
  sec. 17 (restoration appendix included); battery + pixel checks
  green on the round-4 binary.
- **Design round 5 (2026-10-26):** the parked Figma side-by-side is
  closed as a numeric reconciliation - Figma MCP file reads stayed
  paywalled (Starter/View seat), so secs 1-17's verified node reads
  were compared pixel-wise against a fresh capture (cmux_live_7.png,
  restored session): 23/23 checks pass, the three owner divergences
  EXPECTED, zero renderer drift vs the round-4 capture (title strip
  and terminal zones byte-identical), and the residual ledger is
  reconciled (two families moot with owner removals, seven standing).
  No code changed. Evidence: figma-design-reference.md sec. 18; scan
  scripts in %TEMP%\opencode (owner disposal).
- **Owner round 6 (2026-10-26):** the `cmux-win11` status chip is no
  longer drawn ("okay remove it") - `draw_status_bar` re-anchored the
  UTF-8 + `pipe:` leaf segments to the strip's right pad - and the
  strip leads with the active workspace's identity segment
  `w1 · powershell` (minted `w{N}` id + live name, secondary ink,
  then a muted separator) so the blue focused marker visibly tracks
  workspace switches (the live name alone folds to `powershell` in
  every workspace - the minted id is the stable tracker). A fold
  matrix (`status_ws_fit_case`) flexes the pipe-leaf tail (14 -> 8
  codepoints) first, then the ws name; the minted id never folds.
  Evidence: figma-design-reference.md sec. 19;
  behavior probe 14/14, side-by-side scanner 24/0, battery green on
  the round-6 binary (workspace tests 372/0 UNPIPED); drift
  live7->live8 = 6524 px, all in the status bar.
- **Owner round 8 (2026-10-27):** the status bar's tmux-style pane
  marker run (`1:powershell*`, the design's per-pane markers) is no
  longer painted - owner "in the status bar remove 1:powershell*",
  executed with "keep going" (deliberate mock divergence, recorded in
  figma sec 21 along with the sec-20 residual decisions). The strip
  is now: ws identity (id + live name) | CPU/MEM/CLAUDE telemetry |
  right cluster (pipe leaf + UTF-8); the fold matrix lost its marker
  term (`status_ws_fit_case` free-checks [ws + leaf] only) and the
  a11y label still counts the workspace's real panes. Same round
  executed the sec-20 residual the recommended way: NEW browser
  leaves land on a token-painted data-URL home (`browser_home_url`
  rederiving from the resolved viewport fill; restored leaves keep
  their saved URL - old `about:blank` leaves stay), while terminal
  cells stay the Ghostty emulator defaults and the chrome typeface
  stays the current family (both recorded keeps). Evidence:
  figma-design-reference.md sec 20-21; gates fmt/clippy 0, workspace
  tests 372/0 UNPIPED (2 marker pins retired, 1 home-URL pin added),
  build 0; m51 battery 99/99 on the rebuilt binary; ws probe 15/15
  with the marker-retirement predicates; live census cmux_live_11:
  on-token 97.02%, strip info-blue 0 px, drift vs live_10 entirely
  inside the strip band.
