# ADR-0011: Notification pipeline (M3) — OSC ingress seam and slice boundaries

- **Status:** Accepted (owner, 2026-10-18 — M3.1 landed; later slices amend/supersede as scoped)
- **Date:** 2026-10-18
- **Supersedes:** none
- **Related:** ADR-0003 (ConPTY via portable-pty), ADR-0004 (eframe/egui_tty), ADR-0005 (IPC named pipes), ADR-0008 (config), ADR-0010 (workspace tabs)

## Context

M3 milestone ("Notifications + agent hooks + screen detection") needs a source
of desktop notifications. Upstream cmux gets them three ways:

1. **OSCs emitted by agent/shell programs** — `ESC ] 9;body` and
   `ESC ] 777;notify;title;body` (BEL or ST terminated). On macOS the Ghostty
   engine raises `GHOSTTY_ACTION_DESKTOP_NOTIFICATION` with `(title, body)` —
   the app does no wire parsing on the local path
   (evidence: `cmux-main/Sources/GhosttyTerminalView.swift:3561-3576`,
   `cmux-main/Sources/GhosttyDesktopNotificationRequest.swift`).
2. **Agent hooks** (Claude Code `--settings` shim; Codex `hooks.json`) that
   invoke `cmux hooks …` CLI commands over the app socket.
3. **Prompt-line turn detection** (`PromptLineTurnDetector` +
   `PromptTurnNotificationHandler`) for agents without hooks.

Port-side feasibility evidence (verified this session): `egui_tty` 0.2.0 has
no notification surface, and `libghostty-vt` 0.2.2 consumes OSC 9/777 without
exposing an event API. The port owns the PTY byte reader (`start_shell`
reader thread → `mpsc` channel → `egui_tty::Terminal`), so the bytes flow
through code we already own.

## Decision

### M3.1 (landed 2026-10-18): OSC ingress + workspace ring

- **Ingress seam: scan PTY bytes on the reader thread, pre-feed, no strip.**
  A `NotificationScanner` state machine (`cmux-shell/src/notifications.rs`,
  a lib module with its own tests) recognizes every complete `OSC 9` /
  `OSC 777;notify;…` sequence (BEL or ST terminated) in arbitrarily
  fragmented buffers; recognized events ride a second channel while the raw
  bytes still reach the terminal untouched. The engine ignores OSC it does
  not render, so no stripping is needed.
- **Recognition semantics mirror upstream's byte-exact parser**
  (`cmux-main/Packages/macOS/CmuxRemoteSession/Sources/CmuxRemoteSession/
  RemoteTmux/RemoteTmuxNotificationOSCFilter.swift`): selector matched
  byte-exactly (`9;`, `777;notify;` — `09;` is not a notification), bare ESC
  inside a candidate is malformed (candidate abandoned, nothing opens), a
  held ESC followed by ESC re-holds, 4096-byte ceiling abandons a lost
  terminator while keeping the machine alive.
- **Ring model: one unread count per workspace** (`App::workspace_alerts`,
  index-aligned with `workspaces.items`), raised on every drained
  notification event (which pane the event came from is a workspace
  attribute; hidden workspaces are covered because their panes keep
  draining). Viewing a workspace is its mark-read (`Select…`/
  `CycleWorkspace` clear the slot; upstream's workspace-scoped `markRead`).
  The sidebar row shows a warn-colored dot, name shifted right of it.
- Body content stays out of logs (only workspace index + title echoed).

## Amendment — M3.2a (2026-10-18): the arrival policy landed

Upstream's resolver and effects model are now ported one-for-one
(`cmux-shell/src/policy.rs`, evidence anchors in its module docs):

- `NotificationPolicyEffects` → `PolicyEffects`
  (record / markUnread / reorderWorkspace / desktop / sound / command /
  paneFlash — every effect on by default, `all_suppressed()` for the mute
  gate; upstream: `NotificationPolicyEffects.swift`).
- `TerminalNotificationDeliveryDecision.resolve` → `policy::resolve` with
  the same gate order: muted → all-suppressed; app ∧ active-workspace ∧
  focused-surface → focused inline (no banner, no flash, workspace stays
  put, quiet unless `sound_when_focused`); otherwise external delivery —
  with `suppress_when_app_focused` skipping only the banner while keeping
  sound/flash/command. Both settings default `false` upstream
  (`NotificationsCatalogSection.swift`); the port passes those defaults
  until the toast slice adds the config keys.
- Wiring: every drained event resolves through `policy::resolve` with the
  live context (app focus read per frame from egui's `InputState.focused`;
  workspace/pane focus from the model). The ring realizes `mark_unread`
  (upstream keeps unread on the focused path too — the pane shows its own
  ring); the log line records the full disposition so the smoke contract
  can assert it (`cmux-shell: OSC notification in workspace N → external
  ring=… flash=… desktop=…`).
- Machine evidence: in the headless smoke the window legitimately reports
  no keyboard focus, so real events resolve `external` (banner effects
  armed). The focused-surface gate and the suppress detour are covered by
  the 8 unit tests mirroring the upstream truth table.
- Realized today: ring + record log. Deferred to M3.2b: toast emission
  (AUMID decision pending), sound, pane flash, workspace reorder,
  config keys for the two settings, and a mute state.

## Amendment — M3.2b (2026-10-18): the desktop effect = real Windows toasts

The owner picked the recommended AUMID approach. Land it as its own crate:

- **`cmux-toast` (new workspace crate) is the Windows-specific boundary.**
  `ToastDelivery::show(title, body, silent)` queues a post onto a lazily
  spawned worker thread (one MTA `CoInitializeEx` per worker, never on the
  UI thread — the shell pays only a channel send). Worker failures log
  `cmux-toast: …` on stderr and disable delivery gracefully; the ring
  remains the notification of record, matching the upstream principle that
  a toast is one of seven effects, not the pipeline itself.
- **Identity = the documented Windows contract for unpackaged apps**, on
  the `windows` crate 0.62.2 directly (the `tauri-winrt-notification`
  candidate from the earlier table is dropped — feature drift left it
  unnecessary): `IShellLinkW` → `IPersistFile::Save` writes the per-user
  shortcut `%APPDATA%\Microsoft\Windows\Start Menu\Programs\cmux-shell.lnk`
  (target/icon = the running exe, description = `cmux`, the attribution
  line macOS shows from its app name), then
  `SHGetPropertyStoreFromParsingName` (GPS_READWRITE) stamps
  `System.AppUserModel.ID` = `cmux.shell` into the *saved file's* property
  store. Idempotent by checking the stored property first (repair on
  mismatch/absence).
- **Post path**: generic `ToastGeneric` XML assembled by pure tested
  helpers (`xml::escape_text` guards terminal-derived titles/bodies) →
  `XmlDocument` → `ToastNotification` →
  `ToastNotificationManager::CreateToastNotifierWithId(aumid)`. No
  activation handler: Windows routes unpackaged-activation to a foreground
  activator we do not register (post-MVP).
- **Policy realization**: `effects.desktop` gates the post; `effects.sound`
  maps to the toast audio (`sound=false` ⇒ `audio silent="true"`); the
  external path keeps the default notification sound. Toast title = OSC
  title, else `Workspace N` (OSC 9 carries no title upstream either). The
  two `notifications.*` config keys (both default `false`, as upstream)
  now flow from `RuntimeSettings` into `DeliveryContext` — closing
  M3.2a's deliberate placeholder.
- **Bug recorded**: first smoke crashed `0xC0000374` (heap corruption):
  `PropVariantClear` frees `VT_LPWSTR` strings with `CoTaskMemFree`, but
  the written variant's buffer was `Vec`-backed — resetting the variant
  manually fixes it. API-shape evidence (feature gates, `ShellLink`
  coclass GUID without the `CLSID_` prefix, `PKEY_AppUserModel_ID` hidden
  under `EnhancedStorage` — now defined locally by GUID 9F4C2855-… / pid 5)
  was verified against the 0.62.2 registry sources, per the port's
  verify-current-API rule.
- **Machine evidence**: `cargo test` 123 green (+8); smoke with a live
  pane printing `OSC 9` + `OSC 777;notify;Agent 42;cmux toast smoke`:
  `cmux-toast: ready (AUMID cmux.shell)`, two `cmux-toast: posted` lines,
  exit 0, and `Get-StartApps` independently lists `cmux-shell → cmux.shell`;
  the owner saw both banners on the real desktop. Limit: the dev-build lnk
  points at the debug exe until an installer slice exists.

### Later slices (each gets its own decision record or an amendment of this one)

- ~~**Agent hooks (M3.3)**~~ → landed as Amendment 3 below.
- ~~**Claude hooks (M3.3b)**~~ → landed as Amendment 4 below.
- ~~**Typed-claude PATH shim (`hooks claude launch` wrapper verb)**~~ →
  landed as Amendment 5 below.
- ~~**Prompt-line turn detection**~~ → landed as the M3.4 amendment
  below (detector + runtime + delivery wiring; acceptance closed 2026-10-20
  with a 12/12 smoke battery — the pane-child freeze behind earlier
  instability was root-caused to the uncontained pane tree and fixed via
  per-pane job objects, ADR-0003 amendment).
- **Notification history persistence / feed UI, sounds, burst coalescing,
  remote/SSH origins**: post-M3 or post-MVP.

## Amendment — M3.3 slice A (2026-10-19): hook ingress over the control pipe

The direct-CLI hook transport is landed and verified end to end
(ADR-0005 Amendment 1 for the transport itself; this records the
notification-pipeline integration and the acceptance-relevant divergences):

- **Ingress: the pipe handler queues into an mpsc; `poll_children` drains it
  through the same `policy::resolve` pipeline as OSC notifications.** One
  event = one `ValidatedEnqueue` (agent, subcommand, payload, routed
  environment allow-list). The event's `CMUX_SURFACE_ID` (panes get
  `CMUX_SURFACE_ID`/`CMUX_WORKSPACE_ID`/`CMUX_SOCKET_PATH` injected into
  their PTY environment, `identity_spec`) routes it to the workspace that
  owns that pane; a surface the app no longer knows logs
  `unknown surface (no ring)` and drops without a ring slot. Effects are
  realized exactly as for OSC events: ring, desktop toast (title
  `cmux <subcommand>`, body the payload's `message` when present, bounded),
  disposition logged with ring/flash/desktop booleans.
- **Focus regression fixed and locked**: "focused surface" now compares
  against the workspace's focused pane id (`surface_focused`), not the
  event's own pane id against itself — the old OSC-path code marked every
  pane in the active workspace focused. Two unit tests pin the predicate.
- **The CLI is the admission front (divergence from upstream, documented):
  upstream gates in the hook's shell guard (`[ -n "$CMUX_SURFACE_ID" ]`,
  disable var, spool fast path); the port's hooks are inline commands
  (`"<abs path>\cmux.exe" hooks enqueue codex <sub>` — no spool scripts),
  so the guard lives in `cmux-cli`: absent `CMUX_SURFACE_ID` or
  `CMUX_<AGENT>_HOOKS_DISABLED=1` fail open (`{}`, exit 0) without touching
  the pipe; unknown agents fail open; any server rejection (ancestry,
  validation) also fails open `{}` — the agent runner reads stdout as the
  hook result and must never see an error object, and a broken integration
  must never stall a run. `ping` is the probe (exit 1 unless `ok:true`).
- **Installer (`cmux hooks codex install|uninstall`)**: plan preview + y/N,
  plan computed from `CodexHooks` (`port/crates/cmux-hooks` — merge/prune,
  ownership matcher, canonical hash, trust entries, marker-block editor for
  `config.toml`). Feed events (7) are NOT installed this slice — admission
  events only (SessionStart/UserPromptSubmit/Stop; upstream parity for the
  label/matcher plumbing stays in the catalog). When `config.toml` does not
  exist it is not fabricated (upstream guard); trust hashes are canonical
  JSON (serde compact sorted keys) — port-internal consistency only; the
  Windows-codex hooks-runner trust semantics are owner acceptance, not
  oracle-verified.
- **Machine evidence**: `cargo test` 164 green; live smokes: ping 14 ms;
  in-pane (descendant) ping/enqueue through the *default* per-user pipe with
  the ancestry gate on; pane env dump shows the three injected keys;
  non-descendant ping → `unauthorized_peer` exit 1, non-descendant enqueue →
  `{}` exit 0; invalid JSON payload → `{}` exit 0; focused pane + unfocused
  app → `external` + ring + toast posted; unknown surface → no ring + toast;
  app-focused event → `focused-inline`, ring, no toast; codex installer
  round trip restores the user's `config.toml` byte-identically and leaves
  `hooks.json` as `{}`.

## Amendment — M3.3b (2026-10-19): Claude hook parity (owner-approved full event table)

Owner decision: reproduce upstream's Claude integration "the same as
cmux-main"; where a spawn-only transport is behaviorally equivalent, it was
pre-approved. This amendment records what landed and every divergence, with
the evidence basis in `docs/research/rea-findings.md` (Q6).

**Landed:**

- **Settings-document generator (`cmux-hooks::claude`)** — a one-for-one
  port of upstream `emitClaudeWrapperInjectSettings`
  (`CLI/CMUXCLI+ClaudeHookSettings.swift:9-151`):
  `{"preferredNotifChannel":"notifications_disabled","hooks":{…}}` with
  upstream's exact table — queued groups (timeout 5 s) for
  SessionStart→`session-start`, Stop→`stop`, StopFailure→`stop` (alias),
  SessionEnd→`session-end`, Notification→`notification`,
  UserPromptSubmit→`prompt-submit`; Stop also carries a queued `feed`
  group and async `auto-name` (120 s); async `inbox-wait` (86 400 s,
  asyncRewake — verified native in claude.exe 2.1.302) rides
  SessionStart/Stop/StopFailure; `inbox-drain` (5 s) trails every prompt
  submit; PreToolUse carries the `CronCreate` guard plus queued
  `pre-tool-use`; PostToolUse carries queued `push-notification` under
  matcher `PushNotification`; PermissionRequest keeps the direct
  `hooks feed --source claude` decision hook (125 s, synchronous).
  Serialization matches upstream (compact JSON, sorted keys, slashes
  unescaped) and no settings keys beyond upstream's two are fabricated.
- **CLI surface**: `cmux hooks claude inject-settings` (no-socket verb —
  the wrapper's document stage), `hooks feed --source <agent>` (funneled
  through the admission queue), and fail-open `{}` stubs for
  `hooks claude inbox-drain|inbox-wait|auto-name|cron-create-guard` until
  their app-side backends land (agent messages / teams are M4+; upstream
  marks them async or non-decisional, so the stubs cannot stall or rewake
  the agent).
- **Spawn-time injection (the port's wrapper transport)**: a pane whose
  program is the claude CLI (`claude`, `claude.exe`, `claude.cmd` — file
  stem match) is spawned with `--settings <doc>` appended. The
  content-addressed doc lives in `%LOCALAPPDATA%\cmux\claude-settings\cmux-claude-<sha256[:8]>.json`
  (upstream naming). Gates keep plain-claude behavior wherever the doc
  would be inert: IPC off (no pipe identity in the pane env),
  `CMUX_CLAUDE_HOOKS_DISABLED=1` (forwarded to panes via the managed-env
  pattern whenever the app environment carries it), a restored spec
  already carrying `--settings`, or no `cmux.exe` beside the app (the
  port does not bundle a CLI yet). Every failure path degrades to plain
  claude with a log line.
- **Admission**: `claude` joins `KNOWN_AGENTS` and the catalog (a
  port-side `CLAUDE` def — upstream has no `AgentHookDef` for claude; the
  def exists for the enqueue disable-var gate and command-ownership
  classification, and documents claude's own `settings.json` as
  never-written). Ownership covers both `hooks enqueue claude <sub>` and
  the verb form `hooks claude …` (upstream's legacy matcher likewise owns
  `cmux hooks <agent> …` for every verb).
- **Journal-route split (notification pipeline)**: upstream keeps the
  tool-loop feed route out of the notification policy; the port mirrors
  that — subcommands `feed` and `pre-tool-use` log their disposition with
  ring=false/flash=false/desktop=false and touch nothing visual (the log
  line reports effective effects); unrecognized subcommands stay
  deliverable. Delivered subcommands flow through the same
  `policy::resolve` pipeline as slices A/3.2 (ring, toast
  `cmux <subcommand>`, disposition).

**Divergences (all documented; equivalence argued per divergence):**

1. **Exec-form hook commands** instead of upstream's shell-form strings:
   upstream's handler strings are `sh`-scriptable
   (`"${CMUX_CLAUDE_HOOK_CMUX_BIN:-cmux}" hooks …` inside the spool shell
   guard); on Windows the shell-form path is Git-Bash-or-PowerShell, so
   the POSIX guard is not portable. Claude's documented **exec form**
   (absolute `.exe` path + `args` array) spawns "with no shell" on every
   platform, which removes the shell question entirely; the port's CLI
   performs the surface-id/disable-var gating and fails open (as in slice
   A). The `inbox-drain` shell fallback folds into the CLI verb (any
   failure prints `{}`).
2. **Spawn-time interception** instead of upstream's per-pane PATH shim +
   `cmux-claude-wrapper` process: panes whose program is claude get the
   doc directly. `claude` *typed inside an existing shell pane* does not
   yet reach the doc (no PATH shim) — the pre-approved equivalent-behavior
   gap for this slice, closed the same day by Amendment 5.
3. **No spool fast path**: upstream's content-addressed spool scripts are
   not reproduced (the M4 contract carried over from slice A's amendment).
4. **No `--session-id` fabrication**: upstream's wrapper launches claude
   with a generated session id; the port lets claude generate its own
   (hook payloads carry `session_id` regardless).
5. **Permission decisions**: `hooks feed --source claude` funnels through
   the admission queue — no app-side decision — so PermissionRequest
   proceeds with claude's native prompt (upstream answers it from the
   cmux UI; M4+).

**Machine evidence** (this date, this box): `cargo test` 173 green, fmt +
clippy `-D warnings` clean. Live smoke with a `claude.cmd` probe as the
pane program under temp `CMUX_SESSION_FILE`/`CMUX_CONFIG_FILE`/
`CMUX_SOCKET_PATH`/`LOCALAPPDATA` and config
`"terminal":{"shell":{"program":…claude.cmd}}`: probe argv became
`claude.cmd --settings …\claude-settings\cmux-claude-29168d16.json`; the
doc file is byte-identical to `cmux.exe hooks claude inject-settings`
(content-addressed name); pane env carried `CMUX_SURFACE_ID=p1`,
`CMUX_WORKSPACE_ID=ws-1`, `CMUX_SOCKET_PATH`;
`hooks enqueue claude stop` replied `{"id":"hook","ok":true,
"result":{"queued":true}}` exit 0 and the app logged
`agent hook claude stop in workspace 1 → external ring=true flash=true
desktop=true`; with `CMUX_CLAUDE_HOOKS_DISABLED=1` the probe argv carried
no `--settings`, no doc was written, and the pane env carried the
forwarded disable var; `auto-name`/`cron-create-guard` stubs print `{}`.
Real-claude acceptance, run the same day as a pair of zero-cost probes
(owner-approved; design stacks independent zero-cost guarantees: blocking
SessionStart hook, empty `CLAUDE_CONFIG_DIR`, no credential vars, dead
`ANTHROPIC_BASE_URL`):
1. probe doc (exec-form `cmd.exe /d /c hook.cmd`, marker + `exit /b 2` on
   SessionStart): the hook marker was written — claude 2.1.302 loads hooks
   from a `--settings` document and fires them in a real session boot,
   without a trust dialog (the `--settings` trust-gating unknown is
   closed); claude then reached its own `/login` gate — no API call.
2. the port's real generated doc end to end (app under temp env, pane
   types `call claude -p "hi"` through the M3.3c shim): claude applied
   the document without a single settings error, and the app log shows
   the full pre-API + turn-end lifecycle arriving through the pipe —
   `session-start`, `prompt-submit`, `stop`, `session-end` — each with
   `external ring=true flash=true desktop=true` and real desktop toasts
   posted (`cmux session-start`, …). Not fired and not expected: tool
   and subagent hooks (no tools ran), `StopFailure` (claude classified
   the unauthenticated turn end as `Stop`).
   Post-observation fact for isolation models: claude 2.1.302 redirects
   *state* to `CLAUDE_CONFIG_DIR` but still merges the legacy
   `%USERPROFILE%\.claude\settings.json` (read-only; the port doc has
   no `model` key). Interactive live-workspace firing (StopFailure,
   PermissionRequest feed, tool hooks) is observable during the owner's
   normal cmux sessions — no dedicated probe needed.

## Amendment — M3.3c (2026-10-19): typed-claude PATH shim (`hooks claude launch`)

Amendment 4's follow-up, landed the same day: `claude` *typed inside any
pane shell* now reaches the settings document through a PATH shim — the
upstream `agentCommandShims` equivalence, same gates and pass-throughs as
upstream's wrapper:

- **Shared shim dir** `%LOCALAPPDATA%\cmux\claude-settings\shims\`
  holding one `claude.cmd` (`"…cmux.exe" hooks claude launch %*`, CRLF),
  installed at every managed-pane spawn by the same gates as the doc
  injection (`claude_shim_for_panes`: IPC on, disable var unset, sibling
  CLI resolved; any failure keeps the plain PATH with a log line).
  Divergence: upstream ships per-surface uuid dirs for isolated parallel
  sessions; the port's deterministic pane ids need no such lifecycle, so
  one shared dir serves all managed panes — the disable variable still
  prevents the install (and the shim never resolves when absent).
- **`hooks claude launch [claude args…]`** — the wrapper verb. Inside a
  pane with a live cmux (surface id + ping, `CMUX_CLAUDE_HOOKS_DISABLED`
  unset) it stores the content-addressed settings doc (same generator as
  M3.3b), then execs the real claude as `claude --settings <doc> <args>`,
  waiting and forwarding its exit code. Everywhere the doc would be inert
  it pass-throughs plain `claude <args>`: no surface id, disable var set,
  dead cmux (ping failure), doc unwritable, settings dir unresolvable.
  An unresolvable real claude exits 127 loudly. Re-entry proof: the
  real-claude PATH scan skips the shim dir itself, and mimics Windows
  name resolution (directory-major; `.exe` → `.bat` → `.cmd` inside one
  dir — unit-pinned).
- **Bare-name pane programs** (`terminal.shell = "claude"`) are left to
  the shim instead of spawn-time injection — injecting in both places
  would stack two `--settings` flags (eligibility rule + tests).
- **Pane PATH is routed explicitly in identity** (always the app's live
  PATH; shim slot prepended only when enabled): the M3.3c smoke observed
  that portable-pty composes an un-overridden child PATH from the
  default env block (registry casing/content) — app-side PATH mutations
  never reached panes, while every other variable propagates unchanged.
  Explicit routing makes pane PATH deterministic in all gate states and
  fixes that propagation gap beyond the shim.
- **Documented risk (Windows cmd):** `%*` forwards the raw argument text
  but is not a quoting engine — claude arguments containing `%var%`,
  `^`, or pathological quote nesting can mangle when claude is typed
  inside a *batch* chain (PowerShell-typed arguments are unaffected).
  Upstream's zsh shim carries the equivalent POSIX caveats; surfaced
  divergence only if observed in the wild.

**Machine evidence** (this date, this box): `cargo test` 178 green (+5:
shim install/CRLF, PATH-prepend idempotence, real-claude scan
order/skip, identity PATH routing and the always-route fallback), fmt +
clippy `-D warnings` clean. Live smoke with a fake `claude.cmd` (argv
dump, `exit /b 7`) on the app PATH and a runner pane typing
`call claude --probe`: the shim was installed and the fake received
exactly `--settings <doc> --probe` (one flag) with `exit=7` forwarded
through the whole shim→verb→claude chain; with
`CMUX_CLAUDE_HOOKS_DISABLED=1` no shim dir existed, the fake received
`--probe` only, and the exit code still forwarded; the verb outside a
pane pass-throughs unchanged; the pane PATH verified to carry the shim
dir (A) and the app's live PATH (B). Zero-cost real-claude acceptance
ran the same day (see Amendment 4's closure note): the shim chain, doc
application, and hook firing are verified with claude 2.1.302 itself;
no API-spending probes.

## Amendment — M3.4 (2026-10-19): prompt-line turn detection (`turns.rs`)

**Scope.** Ports the *local* half of upstream's turn detection — the Swift
`PromptLineTurnDetector` (semantics + builtin table) that watches a pane's
screen stream for an agent's waiting prompt and fires a turn-complete event.
The *remote* half (the herdr Rust plugin / screen-scan detector,
feature-parity row 10's original reference) stays Post-MVP. Builtin table =
Ollama only, byte-equal to
`CmuxTaskManagerCodingAgentDefinition+BuiltIns.swift` (prompt `">>> "`,
suffix `"Send a message (/? for help)"`, 500 ms completion delay).

**Clean reimplementation, not a translation.** The Swift app code is
GPL-3.0-or-later, so the Rust detector is written fresh against the
*semantics*, validated by porting upstream's 17-test
`PromptLineTurnDetectorTests` suite 1:1 (byte-exact chunk inputs and
assertions). Hard-won semantics preserved: the stored line persists until a
CR/LF/CSI-`G`/`1G`/`2K` boundary; `readyDetector()` leaves `">>> "` stored;
printables unconditionally invalidate a pending turn but a line continuation
ending exactly on a waiting-prompt line re-creates it; `awaitingPrompt(false)`
never fast-path-skips; the overflow latch keeps
`overflowed_line_had_visible_content = visible_byte_count > 0`; bare `CSI K`
is inert; CSI params cap 12; OSC ends at BEL or ESC-\.

**Runtime model mirrors upstream's tee** (`TerminalOutputTeeContext`): a
per-agent `TurnRuntime` confirms deadline-passed boundaries before consuming
the next chunk, so a prompt seen but not yet confirmed survives subsequent
output; events are `TurnEvent::{Pending(TurnPending), Invalidated {
identifier }}`.

**Wiring (cmux-shell).** The `start_shell` reader now emits
`Vec<PaneEvent>` — enum `{ Notifications(Vec<OscNotification>), Turn(TurnEvent) }`
— so one channel carries both ingress kinds. A reader thread cannot make an
idle egui frame loop observe what it just sent, so every channel send calls an
injected `WakeFn` (`Arc<dyn Fn() + Send + Sync>`); the shell crate stays
egui-free, and the app installs a `FrameWake` (Mutex-held `egui::Context`,
installed on the first frame via `App::ui`, shared by every spawn path
including restore) that calls `request_repaint()`. `Pane` holds
`pending_turns: HashMap<&'static str, PendingTurn>` +
`delivered_turns: HashMap<&'static str, u64>`; the frame loop drains the
receiver in `poll_children(ctx)` (Pending supersedes an earlier pending for
the same agent via `is_none_or` + `request_repaint_after(deadline - now)`;
Invalidated removes it), and a deadline sweep delivers through the shared
`policy::resolve` pipeline (ring via `workspace_alerts`, toast with
`agent_name`/"Task completed", decision log) exactly once per agent turn
(high-water mark), matching upstream's single-delivery guarantee.

**Divergence (documented, not silent):** upstream verifies the *foreground
PID* before delivering a turn; ConPTY exposes no foreground-process-group
query, so the port guards with pane-child liveness + the per-agent
exactly-once high-water mark instead. A same-agent re-prompt before delivery
is still superseded correctly; the residual risk (a turn delivered while the
detecting pane is unfocused-but-alive) matches upstream's own fallback
behavior when its foreground check cannot run.

**Verification status (closed 2026-10-20; 12/12 smoke battery PASS, on-time
delivery in every run).** 31 detector-slice unit tests green (`turns.rs` 30 +
`emulator_replies.rs` 1 — 17 upstream-ported byte-exact + fragmentation +
config-rejection + runtime tee-rule tests + `builtin_definitions_match_upstream`
+ a ConPTY 255-byte resize-repaint regression that completes instantly + the
full smoke hand-over sequence through a real `TurnRuntime`,
`smoke_handover_sequence_announces_exactly_once` + the emulator-reply
exoneration) plus 2 live tree-containment tests (`ADR-0003` amendment, in
`cmux-pty-port`); fmt +
clippy `-D warnings` clean; workspace total 211. Three blockers behind the
earlier instability were root-caused and fixed:

1. *Frame-wake gap (above)* — reader threads now wake the idle frame loop.
2. *Smoke-emitter bug* — a line-terminated waiting prompt can never announce
   a turn (the closing CR invalidates the pending its own accumulation
   opened; Swift's `handleLineBoundary` runs before `resetLogicalLine`); the
   emitter now reproduces a real Ollama hand-over with the waiting prompt
   sitting unterminated.
3. *Uncontained pane tree (2026-10-20)* — the "nondeterministic pane-child
   freeze" was the shell-subtree containment gap from ADR-0003 (ConPTY
   children can outlive a force-killed master and stall); per-pane job
   objects closed it: post-fix, 12/12 battery runs complete the runner
   without a single stall. The earlier "pass rate ≈ 1/3" metric was an
   artifact of the smoke's own fixture (a done-mark left from a previous
   run made the wait loop exit instantly, so the checker read the log
   *before* the ~2 s delivery), not of the app. Delivery is now stamped
   `t=<elapsed>` in the dev-tap (`CMUX_DEBUG_READER`) and measured on-time
   (1.4–2.3 s from app start) in every battery run.

## Consequences

- The scanner + policy + toast crate are pure, unit-tested modules (39 lib
  tests incl. every split
  point of the wire sequences, malformed ESC cases, ceiling recovery) — the
  machine smoke drove real OSC bytes through a live ConPTY pane and the
  ring logged them.
- The ring is deliberately minimal (a counter, not the full indicator
  triage of upstream's store: manual/restored/focused-read kinds are
  post-MVP).
- Alerts are not persisted: a restart starts with a clean ring (upstream
  restores unread state from its history snapshot; acceptable MVP gap,
  recorded here).
- `App::workspace_alerts` is a port-local parallel structure to
  `Workspaces`; the API gap vs. upstream (per-tab notification objects,
  `TerminalNotification` model, feed history) is intentional scope control
  and re-enters exactly when a slice needs richer per-event state (toast
activation/actions, the history panel).
