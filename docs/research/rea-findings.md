# REA findings ledger

Evidence-based investigation notes for the cmux Windows port. REA (MCP server
`rea`) is used for concrete questions that source inspection alone cannot
answer. Each finding records: question, method, evidence, classification
(verified fact / strong inference / hypothesis / unknown), and design impact.

Format: one entry per question, newest last.

---

## Q1 — Is REA installed, and which analysis methods does it support?

- **Method:** MCP tool discovery via the OpenCode tool catalog (`search` over
  namespace `rea`) and `list_mcp_resources`.
- **Observation:** REA is connected with **207 tools**. Confirmed families:
  native binary analysis with provider binding (`open_binary`, `binary_overview`,
  `batch_decompile`, `analyze_function`, `get_call_graph`, Ghidra/Hopper
  providers), managed (.NET) inspection, JavaScript/ASAR application analysis,
  Swift symbol analysis (`analyze_swift_types`, `demangle_swift`), browser and
  Electron runtime observation (`capture_browser_scenario`,
  `capture_electron_scenario`, `inspect_web_page`), network capture inspection,
  and process capture (`capture_process_scenario`).
- **Resources:** `list_mcp_resources(server: "rea")` returns 0 resources — the
  server exposes tools only, no documents.
- **Classification:** Verified fact (direct tool listing, 2026-10-17).
- **Design impact:** REA can support static binary analysis, runtime observation
  on Linux/macOS targets, and JS/browser evidence gathering. Method choice must
  be confirmed per question (Q2 shows a verified negative).

## Q2 — Can REA capture Windows PTY process behavior (ConPTY)?

- **Question:** What does ConPTY emit for a trivial shell command on this
  machine (title OSCs, cursor-position requests)? This informs whether the
  vertical slice's emulator layer must handle ConPTY re-encoding.
- **Method:** `tools.rea.capture_process_scenario` with
  `executable: cmd.exe, arguments: [/c echo CMUX_PORT_PROBE_OK]`,
  `terminal: {columns: 80, rows: 10, scrollback: 100}`.
- **Observation:** Typed error `capability_unavailable` /
  `unsupported_provider` from provider `rea-process`:
  > "Windows PTY process capture is unavailable because this adapter does not
  > yet verify descendant cleanup. Use Linux (including WSL for Linux commands)
  > or macOS. Reinstalling the PTY backend does not enable native Windows
  > capture."
- **Classification:** Verified fact (server-reported capability boundary,
  2026-10-17). This matches the governing AGENTS.md rule: never assume an REA
  analysis method until confirmed.
- **Design impact:** ConPTY runtime behavior cannot be observed through REA on
  this machine. ConPTY evidence must instead come from the vertical slice's own
  instrumentation (log the PTY byte stream) or a WSL/Linux capture. Secondary
  evidence already available from public sources: wezterm documents that ConPTY
  re-encodes output and injects sequences such as OSC title changes and
  `\e[6n` cursor-position requests that the emulator must answer
  (wezterm/wezterm discussion #2459, issue #6783).

## Q3 — Can REA analyze Ghostty binaries locally?

- **Method:** File search for built artifacts in the reference snapshot.
- **Observation:** No `GhosttyKit.xcframework`, `libghostty-vt.a`, or other
  built binaries exist in `cmux-main/` — only
  `cmux-main/scripts/ghosttykit-checksums.txt`. The `ghostty/` submodule
  directory is empty (snapshot has no `.git`, so submodules were never
  initialized).
- **Classification:** Verified fact (file listing, 2026-10-17).
- **Design impact:** REA native static analysis of the Ghostty engine (and of
  any packaged cmux artifact) is not possible locally until artifacts are
  fetched or built — which requires installing tooling and downloading
  prebuilt frameworks, i.e. approval. Until then, engine evidence comes from
  upstream sources (Q4) and the `cmux-tui` binding crates' build scripts.

## Q4 — Does libghostty-vt support Windows? (secondary research, pending primary verification)

- **Method:** Web research against primary sources (ghostty-org/ghostty issues,
  PRs, discussions; crates.io; community project docs).
- **Observations (all from public sources, 2026-10-17):**
  - A Ghostty collaborator states: "we officially support Windows for
    libghostty-vt. We build and run the tests on Windows" — Windows test run:
    2604 passed, 53 skipped, 51/51 build steps (ghostty-org/ghostty
    discussion #12371, PR #11756, commit a1370d9 for full-libghostty DLL mode).
  - `libghostty-vt` is pure Zig, no C dependencies, no global state init; the
    full `libghostty` pulls in freetype/harfbuzz/glslang/oniguruma and is
    described as "greenfield" on Windows DLL loading.
  - The Ghostty team will not ship an official Windows GUI; community
    libghostty-based Windows terminals are the sanctioned path
    (discussion #12371).
  - Known Windows build quirk: Zig fails building Ghostty's `uucode` helper
    when Zig and the project share a drive letter; workaround documented in
    libghostty-rs issue #9 (run from the cached uucode package dir).
- **Classification:** Strong inference (authoritative statements from Ghostty
  collaborators, not reproduced on this machine — no Zig/Rust toolchain here
  yet, and the ghostty submodule is absent).
- **Design impact:** `libghostty-vt` is a viable terminal-engine candidate for
  the Windows port, aligning with the existing `cmux-tui` binding
  (`ghostty-vt-sys`, Zig 0.16.0). The vertical slice must reproduce the build
  locally to upgrade this to a verified fact. Fallback engine: `alacritty_terminal`
  (proven on Windows by NabiTerm and Paneflow's non-x64 backends).

## Q5 — What does ConPTY actually emit on this machine? (runtime observation via the port itself)

- **Method:** REA process capture is unavailable on Windows (Q2), so the
  observation came from the port's own instrumentation: the
  `cmux-pty-port` probe example and its integration tests drive real ConPTY
  sessions and log the raw byte stream (`examples/pty_probe.rs`).
- **Observations (verified on Windows 10 Pro 24H2, 2026-10-17):**
  - A startup `\e[6n` cursor-position DSR probe appears in the PTY stream
    (observed with portable-pty 0.9.0), matching the upstream wezterm
    report; the emulation layer must answer it.
  - ConPTY re-encodes output: `\e[?9001h`/`\e[?1004h` mode enables,
    `\e[2J` clears, and title OSCs are injected even for trivial `echo`.
  - The output pipe does not EOF when the child exits.
  - Dropping the master writer kills the child (`0xC000013A`).
  - portable-pty 0.9.0 `Child::wait()` hangs while the writer is held
    (upstream regression wezterm/wezterm#6783) — the workspace pins 0.8.1,
    where `wait()` returns the natural exit code. See ADR-0003.
- **Classification:** Verified fact (direct runtime observation on this
  machine).
- **Design impact:** Confirms ADR-0003's lifecycle rules and the reader-thread/
  reply-hook design of the M1 slice; ConPTY byte-stream golden captures (test
  strategy L3) should assert tolerance, not exact bytes.

## Q6 — How do Claude Code hooks behave on Windows (claude.exe 2.1.302), and what does upstream inject for it?

Context: M3.3b (Claude hook parity). Owner directive: reproduce upstream's
Claude feature; use REA where source inspection is not enough.

- **Method (three layers, labeled):**
  1. REA `open_binary` bound `C:\Users\noah\.local\bin\claude.exe` (PE x86_64,
     sha256 `3ab075f7…`). Deep string analysis returned
     `provider_unavailable` (no decompiler engine installed on this box);
     per the REA recovery guide, the remaining observations below are
     **ordinary shell-tool / web evidence, not REA Evidence**.
  2. Direct byte-scan of the exe's embedded JS bundle (PowerShell, Latin-1).
  3. Official hooks reference: `https://code.claude.com/docs/en/hooks`
     (fetched 2026-10-19), plus upstream source reads
     (`cmux-main/CLI/CMUXCLI+ClaudeHookSettings.swift`,
     `cmux-main/CLI/cmux.swift:45418-45424, 31108+`,
     `CMUXCLI+AgentHookAdmission.swift:117-138`,
     `Packages/macOS/CmuxTerminal/.../TerminalSurface+RuntimeSurfaceCreation.swift`).
- **Observations:**
  - claude.exe 2.1.302 event vocabulary (byte-scan, exact list):
    `PreToolUse, PostToolUse, PostToolUseFailure, PostToolBatch, Notification,
    UserPromptSubmit, UserPromptExpansion, SessionStart, SessionEnd, Stop,
    StopFailure, SubagentStart, SubagentStop, PreCompact, PostCompact,
    PreModelSwitch, PostModelSwitch, PermissionRequest, PermissionDenied,
    Setup, TeammateIdle, TaskCreated, TaskCompleted, Elicitation,
    ElicitationResult, ConfigChange, Worktree…` — **StopFailure exists** and
    fires "when the turn ends due to an API error" (official docs table).
  - `asyncRewake` is a native claude feature in this build (byte-scan:
    "runs in the background and wakes Claude on exit code 2 … **Implies
    async**"; docs confirm; `flushPendingAsyncRewakeHooks`,
    `executeStopFailureHooks` symbols present). Upstream cmux's inbox-wait
    hook (async + asyncRewake, timeout 86400) is supported as-is.
  - Hook handler execution (docs): **exec form** (`args` array present) spawns
    `command` as an executable directly — "no shell", "No shell tokenization
    happens on any platform"; Windows exec form requires a real `.exe`
    (`.cmd`/`.bat` shims cannot be spawned). Shell form (no `args`) runs via
    Git Bash on Windows, else PowerShell. `shell` field accepts
    `"bash"|"powershell"`.
  - `--settings <file-or-json>` (CLI help): "Path to a settings JSON file or a
    JSON string to load **additional** settings from" — merged over other
    settings sources, not replacing them. `--session-id <uuid>` exists.
    `--setting-sources` can exclude sources. `-p` mode silently ignores
    settings that fail validation.
  - Hook config keys honored (docs): `type, if, timeout` (seconds; default
    600; lowered to 30 on UserPromptSubmit/Pre-/PostModelSwitch, 10 on
    MessageDisplay), `statusMessage`, `once` (frontmatter only), `async`,
    `asyncRewake`, `matcher` (exact/regex rules; `""` or `*` = all), nested
    `hooks` groups. `hooks` key lives in settings JSON; entries merge across
    settings levels ("add their own hooks without removing" others).
  - Hook trust gating exists (byte-scan symbols: `shouldSkipHookDueToTrust`,
    `hooks_personal_load_check`) — unverified for `--settings`-injected hooks
    on a trusted project; owner smoke will confirm.
  - Stop-loop protection (byte-scan): "For Stop/SubagentStop hooks, check
    stop_hook_active in the input and return success while it's true" +
    `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP`.
  - Upstream injected doc (read verbatim,
    `CMUXCLI+ClaudeHookSettings.swift:9-101`): top-level
    `{"preferredNotifChannel":"notifications_disabled","hooks":{…}}` sorted
    keys, slashes unescaped; queued groups (timeout 5 s = shared
    `declaredTimeoutSeconds`; gate performed in the spool shell guard:
    `CMUX_SURFACE_ID` non-empty AND `CMUX_CLAUDE_HOOKS_DISABLED != 1`, else
    print `{}` and exit 0): SessionStart→`session-start`, Stop→`stop`,
    StopFailure→`stop`, SessionEnd→`session-end`, Notification→`notification`,
    UserPromptSubmit→`prompt-submit`; Stop also gets a queued `feed` group
    and async `auto-name` (timeout 120); `inbox-wait` (timeout 86400,
    async+asyncRewake) on SessionStart/Stop/StopFailure; UserPromptSubmit
    also gets `inbox-drain` (timeout 5); SubagentStop→queued `feed`;
    PreToolUse: `cron-create-guard` (matcher `CronCreate`, timeout 5) +
    queued `pre-tool-use`; PostToolUse: queued `push-notification` (matcher
    `PushNotification`); PermissionRequest→`hooks feed --source claude`
    (timeout 125, sync decision hook).
  - Upstream hook commands are shell-form strings
    (`"${CMUX_CLAUDE_HOOK_CMUX_BIN:-cmux}" hooks …`) wrapped in the spool
    producer with shell-guard fallback
    (`CMUXCLI+AgentHookAdmission`/`AgentHookSpoolProducer.swift:69`: gate
    `${CMUX_SURFACE_ID:-}` non-empty and `${CMUX_CLAUDE_HOOKS_DISABLED:-} != 1`,
    else print `{}` and exit 0). Max hook stdin 1 MiB; fail-open `{}\n`.
  - Upstream wrapper transport (source): per-surface PATH shim dir
    (`agentCommandShims`) + `cmux-claude-wrapper` pings the socket, calls
    `cmux hooks claude inject-settings` (no-socket verb,
    `cmux.swift:45418-45424`), writes the doc, execs real claude with
    `--settings`; app env sets `CMUX_CLAUDE_HOOKS_DISABLED=1` when the
    integration is off (`TerminalSurface+RuntimeSurfaceCreation.swift:159-161`).
- **Classification:** Verified fact for byte-scan/help/docs observations and
  upstream source shapes (static evidence); **RESOLVED 2026-10-19 by the
  zero-cost real-claude probes (ADR-0011 Amendment 4 closure note):**
  `--settings` hooks load and fire in claude 2.1.302 sessions without a
  trust dialog (blocking-hook marker + four lifecycle hooks through the
  live pipe), exec-form handlers spawn with args (every fired hook), and
  shell-form hooks were never needed (exec form is the port's shape).
- **Design impact (M3.3b):** The port emits upstream's exact table but in
  **exec form** (`command` = absolute `cmux.exe`, `args` = verb list) — the
  documented no-shell path that removes Git-Bash-vs-PowerShell fragility and
  makes the shell-guard unnecessary (the CLI performs the env gate itself,
  as in slice A). `inbox-drain`'s shell fallback folds into the CLI verb
  (any failure prints `{}` ex 0). Typed-`claude` interception (per-surface
  shim dir + `hooks claude launch` wrapper verb) follows as a second step;
  spawn-time interception (pane program = claude → `--settings <doc>`)
  lands first.

## Q7 — How does the typed-claude shim behave on Windows, and what does the port's pane env composition do? (M3.3c)

Context: ADR-0011 Amendment 5 (typed-claude PATH shim). Method: live smoke
with a hermetic fake claude + runner pane under temp env vars
(`smoke-m33c.ps1`, `cmux-smoke-claude-typed`); plus a minimal controlled
env-diff probe against the running port (`cb-inherit-probe*.ps1`).
Ordinary-tooling runtime observation — **no REA Evidence involved**.

- **Observations (verified on this box, portable-pty 0.8.1, Windows 11):**
  - The full typed chain works: `call claude …` (typed inside a pane
    shell) → shim `claude.cmd` → `cmux.exe hooks claude launch` → ping →
    content-addressed doc → real claude as `claude --settings <doc>
    <args…>`, with the child's exit code forwarded through
    cmd → shim → verb → pane (`exit /b 7` surfaced as the pane's exit
    status in the app log).
  - `cmd` batch chains: a second batch file invoked **without `call`**
    transfers control (the runner silently ends) — the shim's own
    delegation to `cmux.exe` (an exe) is unaffected, but user-level
    batch runners must use `call claude …` to continue afterwards.
  - **Pane env composition:** with no PATH override in the pane spec,
    the child PATH was composed from the default env block — registry
    casing (`Path=`) and content — NOT the app's live `PATH`: a PATH
    mutation made on the app's environment just before spawn did not
    reach the pane, while all other app-env variables tested
    (`CMUX_SESSION_FILE`, `CMUX_CONFIG_FILE`, `LOCALAPPDATA`) did.
    With a PATH override in the spec the override value reached the
    child verbatim (and identity then reflects the app's live PATH).
  - Residual unknown (owner-verifiable only): real claude behavior —
    `--settings` validation, trust dialog for shim-launched hooks, and
    interactive TUI sizing inside the shim chain.
    **Partially resolved 2026-10-19 by the zero-cost real-claude
    probes:** `--settings` validation + trust gating + the shim chain
    itself are verified with claude 2.1.302 (see ADR-0011 Amendment 4's
    closure note); only interactive TUI workflow remains for the
    owner's normal use.
  - claude-side note for future probes:
    `CLAUDE_CONFIG_DIR` redirects state (`.claude.json`, projects,
    sessions) but 2.1.302 still merges the legacy
    `%USERPROFILE%\.claude\settings.json` — read-only; probes stay
    zero-cost via absence of credentials + dead `ANTHROPIC_BASE_URL`.
- **Design impact (M3.3c):**
  - Shared shim dir (not upstream's per-surface uuid dirs) is sufficient
    with the port's deterministic pane ids; the disable variable keeps
    its gate at install time.
  - Identity **always routes pane PATH explicitly** from the app's live
    PATH (shim slot prepended only when enabled) — determinism in all
    gate states and a propagation fix beyond the shim.
  - Bare-name `claude` pane programs are left to the shim; spawn-time
    injection applies only to explicit paths (double `--settings`
    guard).
  - cmd `%*` forwards raw argument text; `%var%`/`^`/quote-nesting
    mangle risk documented as the Windows analogue of upstream's zsh
    shim caveats.

## Open questions

- None currently assigned to REA. Candidate future questions: compare built
  Windows artifacts of the port against expectations via
  `inspect_rust_binary`; verify browser-pane automation against a live
  WebView2 page via `inspect_web_page`/`capture_browser_scenario` (browser
  tools operate on user-owned pages; WebView2 CDP support needs checking
  against REA's provider list before assuming).
