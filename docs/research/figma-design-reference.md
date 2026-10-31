# Figma design reference: "Cmux Windows Port" (design-fidelity assessment)

*Source artifact:* the owner's Figma file — file key `AGKdhDGDEmQieIRKzc7ain`,
root node `1:2` "cmux — Minimal Terminal Variant", canvas 1280x1024.
Read 2026-10-22 through the Figma MCP (`get_metadata`, `get_screenshot`,
`get_design_context`; skill `figma-design-to-code` loaded). The full
metadata and context exports were retrieved that day; this file records
the extracted spec and the fidelity assessment. Classification per the
standing rules: all values below are **verified facts** read from the
file unless marked *inference*.

## 1. What the design is

One 1280x1024 desktop composition of the port as a finished product,
divided as:

| Region | Node | Geometry |
| --- | --- | --- |
| Workspace sidebar (vertical strip) | `1:399` Aside | x=0, 256 wide, full height minus 32px title row |
| Title row: breadcrumbs "cmux / backend-monorepo > agent-loop", right-side INSPECTOR + window buttons | inside `1:2` | 0..32px tall, full width |
| Command bar: "Quick dispatch, processes or jump to pane..." `Ctrl K` | inside `1:2` | second row under the title row (*inference* ~36px; node list truncated in the read preview) |
| Main area ("Main" `1:3` x=256) with telemetry sub-header `1:5` | 32px tall | cwd chip, git delta chip, agent-state pill, hotkey chips, split buttons, `Reset` |
| Split layout `1:69` | 12-col grid, 4px gap/padding | left column `1:70` cols 1..7 (591px): PANE 1 vitest shell (focused, `1:71`) + PANE 2 git diff inspector (`1:173`, 224 tall); right column `1:244` cols 8..12 (421px): agent pane |
| Status bar `1:371` "Footer - Bottom Workspace Status Bar (Native Windows 11 / tmux minimalist ergonomics)" | 24px tall | pane list, CPU/MEM, CLAUDE state, listener address, UTF-8, product tag |

## 2. Palette (verified from the context export)

| Role | Value |
| --- | --- |
| Main background / pane fill | `#11131a` |
| Terminal viewport (darkest) | `#0c0e14` — also the footer status bar fill (node 1:371 carries `bg-[#0c0e14] h-[24px]` itself; corrected 2026-10-23 from the raw re-read) |
| Sub-header fill | `#191b22` |
| Pane header bar | `#282a31` |
| Chip background (standout) | `#1d1f26`; secondary chip on viewport `rgba(29,31,38,0.3)` |
| Summary box on pane 1 | `#191b22` |
| Accent (focus ring, cursor, dots, SEND) | `#38bdf8` |
| Active-focus pill fill | `rgba(56,189,248,0.2)`; agent-state pill `rgba(79,49,156,0.2)` with dot `#cebdff` |
| Text primary | `#e2e2eb` |
| Text secondary / muted | `#bdc8d1`, `#87929a` |
| Semantic: info blue | `#8ed5ff` |
| Semantic: warning amber | `#ffc42f`, soft `#ffdf9f` |
| Semantic: danger red | `#ffb4ab` |
| Semantic: agent purple | `#cebdff` |
| Focus ring | 1px `#38bdf8` on the focused pane, radius 4 |

Port-side tie-in (verified): the shell's built-in dark chrome is
`background = 0x10141C`, `sidebar_background = 0x0D1016` — the same
dark family (within ~2-6 RGB steps per channel). The design is a
*fidelity pass over the current look*, not a re-theme; the deltas are
 Accent colors and new chrome elements, plus exact-hex adjustments.

## 3. Typography

| Face | Weights | Used for | License note |
| --- | --- | --- | --- |
| `Liberation Serif` | Regular, Bold | nearly all UI text AND the terminal lines (12-13px) | open-licensed (OFL/GPL+exception family); verify exact license before bundling |
| `Inter` | Regular, Bold | uppercase pill labels ("CLAUDE CODE", "ACTIVE FOCUS"), "+" connectors, SEND | OFL |
| `FreeSans` | SemiBold | the shell chevron glyph "➜" | GNU FreeFont (GPL+font exception); verify before bundling |

*Inference:* the pane-body text styling (serif terminals with rounded
per-test chip rows) is the designer's illustrative mockup of terminal
content, not a statement that the terminal engine should render arbitrary
program output in serif; real panes render the PTY's own output in a
monospaced grid.

## 4. Layout metrics worth keeping (verified)

- Window 1280x1024; sidebar 256px; title row 32px; sub-header 32px; status
  bar 24px; split grid gap/padding 4px; pane corner radius 4px.
- Focused pane: 1px `#38bdf8` ring; status dots 6px (sub-header) and 8px
  (pane headers); pane header 23.67px tall (`1:72`).
- Pane header model: `N: title` + middot + metadata snippet (latency/rev)
  on the left, `ACTIVE FOCUS` pill + icon buttons on the right.
- Agent column right footer: "PROMPT / INSTRUCTION DISPATCH" with an
  `Enter to send` hint and a SEND button (44x18).

## 5. Complete string inventory

Title row: `cmux` `/` `backend-monorepo` `>` `agent-loop` | `INSPECTOR`.
Command bar: `Quick dispatch, processes or jump to pane...` `Ctrl` `K`.
Sub-header: `~/dev/cloud-orchestrator` `[main!]` `+12` `-3` `staged`
`CLAUDE CODE` `.` `executing` | `Ctrl` `+` `D` `v-split` `Ctrl` `+`
`Shift` `+` `D` `h-split` | split/img buttons | `Reset`.
Sidebar: `Workspaces`; rows `backend-monorepo`(WAIT), `ai-agent-evals`
(RUN), `infra-deploy` (IDLE), `test-suite-runner` (ERR);
`RUNTIME SERVICES`: `:3000 web-vite UP`, `:8080 api-gateway UP`,
`:5432 postgres STOP`.
Pane 1: `1: zsh - pnpm test:e2e` `.` `24ms` `ACTIVE FOCUS`; shell prompt
line (`➜ cloud-orchestrator on git:main! via v20.12.2`); `$ pnpm vitest
run tests/orchestration.e2e.ts --reporter=verbose`; `RUN v1.6.0 ...`;
four test rows with `14ms/9ms/18ms/PENDING`; summary box (`Test Files`
`1 passed (1)`, `Tests` `3 passed` `1 skipped` `(4 total)`, `Duration`
`2.41s (...)`); prompt with blinking cursor `pnpm test:watch --filter=auth`.
Pane 2: `2: git diff --stat HEAD>1` `.` `rev 4b79c1e`, `+48` `-12` `3
files changed`; commit text block; diff stat bars per file; unified hunk.
Agent column: `3: codegen-v4 (claude-3-7-sonnet)` `STEP 4/7`; step cards
(`AST Analysis Complete 0.42s`, `Dependencies Resolved 1.15s`,
`Generating Spec Tests 1.80s`), active card `STEP 4/7: REFACTORING JWT
VALIDATION` `RUNNING` with code block + `Tokens: 1,842 in / 439 out`
`Speed: 74 tok/sec`; `Pending Patch: src/server/auth.ts` `8 lines
affected` with +/- diff preview; gate `Human Approval Required` +
`Approve & Continue (Y)` / `Modify Diff`.
Status bar: `1:zsh*` `2:git-diff` `3:codegen-v4` | `CPU: 18%`
`MEM: 1.4 GB` | `CLAUDE: ACTIVE` | `127.0.0.1:8080` | `UTF-8` |
`cmux-win11`; the `v0.9.4` version chip sits in the aside's bottom strip
(2026-10-23 re-read correction — it never was footer-left).

## 6. Fidelity assessment (design element -> port state)

| Element | Port state today (verified) | Achievable pixel fidelity | Needs |
| --- | --- | --- | --- |
| Sidebar geometry/rows/close/rings | landed (custom-painted, M2.4b/M3.1) | ~100% | exact hex/dot columns |
| Sidebar status dots + WAIT/RUN/IDLE/ERR | absent (no per-workspace state rollup UI) | achievable | new state rollup (agent-detection feed exists via M3.4) |
| Sidebar RUNTIME SERVICES (ports UP/STOP) | absent — feature-parity row 12 not landed | achievable | row-12 port-listener data |
| Title row (custom dark title bar + breadcrumbs) | native Win11 chrome, no custom bar | achievable | window-lifecycle lane (decorations off + native drag/snap handling) |
| Command bar + palette | absent — row 13 (palette) not landed | achievable | row-13 lane |
| Telemetry sub-header chips | absent; agent-state source exists (M3.4 detector), git status absent (row 11) | achievable | row-11 data for git chips |
| Pane chrome (header bar, dots, radius, focus ring, pills, close buttons) | landed equivalents (headers, close, focus chrome); values differ slightly | ~100% | exact-value pass |
| Terminal pane CONTENT (mock rows, serif body, card summary box) | real PTY output in monospaced grid (egui_tty) | **not a render target** | n/a - content is program output, not chrome |
| Agent activity cards / pending patch / approval gate / prompt dispatch | absent (no card-stream UI; upstream shows agents in terminals + notification rings/M3.2b) | achievable but a new product surface | owner product decision + new lane |
| Status bar | landed (lane 2: 24px main-area strip with pane markers, machine CPU/MEM, agent state, pipe leaf, UTF-8, product chip; aside bottom strip carries the version chip) | ~100% | bar address = the port's control-pipe leaf (honest stand-in for the design's listener) |
| Icons (30+ vectors) | the shell draws a few shapes today | ~100% | rasterize/download assets (egui has no SVG; render to PNG) |
| Fonts/letter-spacing | egui supports bundled TTFs; NO letter-spacing primitive | close (~99%; 0.22-0.55px tracking is sub-pixel) | font bundling + license check |

## 7. Bottom line (assessment, per the owner's question)

- **Static chrome** (every region except pane interiors): the design is
  built from rectangles, text, dots, pills, strokes and shadows that
  egui renders exactly; combined with the palette being a near-match of
  the port's existing dark theme, a verifiable ~100% chrome match is a
  matter of implementation slices (exact tokens, five new chrome
  regions, asset integration), not an architecture change.
- **Dynamic regions** (sidebar data segments, sub-header git chips,
  RUNTIME SERVICES, status-bar addresses) match only as far as their
  parity rows are landed (11/12 remaining; agent state feeds exist).
- **Pane interiors**: the vitest/git-diff/agent bodies are illustrative
  mockups of PROGRAM OUTPUT. A living port renders what shells and
  agents actually emit; the comparable guarantee is "same chrome frame,
  real content", not "the mock's decorative rows reproduced". Any claim
  of literal 100% on the full picture would mean faking terminal
  content, which the port will not do.
- **Verification path** (why a number is provable): the M4/M5 battery
  already decodes shell screenshots off-screen; a parity lane would add
  a design-vs-build sampling check (position/color probes at fixed
  coordinates) against the exported PNG at 1280x1024.

*Recorded residual:* the Figma file carries one screen variant
("Minimal Terminal Variant"); other variants (browser pane look, light
theme, agent-independent layouts) are absent from the file.

*Recommended lane order (proposed, pending owner approval):*
(1) exact-token chrome pass on existing regions (sidebar/pane
rings/labels/format radii) -> (2) status bar + title-bar custom chrome
-> (3) sub-header with git chips (row 11 prerequisite) + command bar
(row 13) -> (4) services sidebar segment (row 12) -> (5) agent activity
surface (product decision required). Terminal-pane content stays live.

## 8. Lane 1 landed: exact-token chrome pass (2026-10-23)

The recommended lane order above feeds the post-M6 plan; step 1 is in
`port/`:

- Defaults now carry the verified palette: main bg/gutters `#11131a`,
  sidebar `#191b22` (fill re-extracted from the saved node tables; the
  section-2 palette omitted the aside), terminal pane interior
  `#0c0e14`, chip `#1d1f26`, accent `#38bdf8` as a 1px focus ring (the
  old 2px default-blue ring is gone), text `#e2e2eb` primary /
  `#bdc8d1` secondary / `#87929a` muted, info `#8ed5ff` (active
  sidebar row name + links), warning `#ffc42f` (unread ring dot),
  danger `#ffb4ab` (failure ink), and the aside hairlines
  rgba(62,72,79,0.4/0.3).
- Unfocused panes stroke nothing (design: gutter-only separation);
  focused panes wear the 1px accent ring, and the viewport fill
  rounds with it at 4px.
- The tokens install into egui's dark visuals at startup, so egui-fed
  inks (terminal cursor = accent, notice/error ink, hyperlinks) match
  without duplicating the palette at every call site.
- Config precedence is recorded: `theme.background` still pins the
  background, the darker sidebar, AND the pane interior (the
  chrome-blends-with-cells contract the shipped template documents);
  the design values are the unconfigured defaults. A configured light
  scheme skips the design visuals override entirely.
- Recorded inferences: hovered sidebar rows wear the soft hairline
  hue (the design states no hover variant), and the ACTIVE row name
  is the info-blue read off the focused workspace's row (node 1:420).
- Live-verified with a one-shot PrintWindow pixel probe (not a saved
  lane): sidebar `#191b22`, active chip `#1d1f26`, viewport
  `#0c0e14`, 1px `#38bdf8` ring, and the hairline blend at the aside
  boundary all match the design at exact RGB. The saved
  design-vs-build sampling lane stays future step-lane work per
  section 7.

## 9. Lane 2 landed: status bar + aside version strip (2026-10-23)

Ladder step 2's status-bar half is in `port/`; the custom title bar
stays open as the next sub-lane (window-lifecycle work: decorations off,
drag/snap, native buttons).

**Status bar** (nodes 1:371..1:398 verified off the raw exports):

- 24px strip across the main area only (the footer x-origin is Main's
  left edge, width 1024 in the 1280 file), fill = the terminal-viewport
  dark `#0c0e14` (node 1:371 carries `bg-[#0c0e14] h-[24px]` itself —
  the section-2 palette had labelled the footer `#191b22`, that was the
  sub-header; corrected above). The aside keeps its full height, so the
  sidebar column no longer ends at the window bottom.
- Left cluster 12px: one tmux-style marker per pane of the active
  workspace (`ordinal:title`, focused carries the trailing `*` — the
  design's `1:zsh*`; corrected from the earlier `1:zsh!` inventory
  note), `#8ed5ff` focused / `#bdc8d1` others, an untitled pane reads
  `–`; then `|` muted, `CPU: n%` and `MEM: x.y GB` muted, and
  `CLAUDE: ACTIVE` info-blue / `CLAUDE: IDLE` muted.
- Right cluster 11px: the port's control-pipe leaf as the honest
  stand-in for the design's listener address (`pipe: <leaf>`, `ipc off`
  when IPC is disabled), `UTF-8` (the constant the port enforces
  through ConPTY), and the `cmux-win11` product chip (primary ink on
  `#191b22` inside the 0.4-alpha hairline, 4px corners).
- Machine data: `GetSystemTimes` CPU% refined over two snapshots every
  `~0.75s` (kernel time already counts idle on Windows), RAM-in-use
  from `GlobalMemoryStatusEx` (machine-wide; sized `GB` one decimal
  below 10GB, whole `GB`/`MB` otherwise — the design's mock gives no
  scope, recorded as an inference). Kernel32-only FFI; no new crates.
- Agent state semantics (inference; the mock shows only `ACTIVE`): the
  active workspace has seen a detected agent session when any terminal
  lane holds a pending or delivered turn-boundary slot (the M3.4
  detector's evidence). Agent-purple pane markers wait for the
  agent-activity lane (no per-pane agent identity yet).
- The whole strip is one hover-sensed a11y region labelled
  `Status bar, N pane(s)` (the M6 custom-paint label precedent).

**Aside bottom strip** (node 1:485 "Background+HorizontalBorder"):
38px tall, fill `#1d1f26`, 0.3-alpha hairline on its top edge, sidebar
workspace rows stop above it. The version chip (`#0c0e14`, 4px corners,
6px info-blue dot — a `rounded-full` info read — with a secondary 12px
`v{CARGO_PKG_VERSION}` label) hugs the right edge. The design's
settings/help icon links on that strip stay unimplemented until the
port has destinations for them (recorded residual); the small address
glyph before the status bar's listener segment is likewise skipped (no
icon font; rasterized assets are a later asset lane).

**Verification**: unit tests cover every label formatter, the CPU
delta math, and the chip/ipc/version shapes (`cargo test` 323 = 319 +
4 new). A one-shot DPI-aware PrintWindow probe verified exact RGB on
the live build: bar fill `#0c0e14` uniform top/bottom rows, strip fill
`#1d1f26`, version chip dark interior + info dot `#8dd4fe`, focused
marker ink `#8ccff4`-family, muted segment ink, and the product chip
(`#191b22` + primary ink) — PASS then the probe was deleted. Battery on
the shipped binary: smoke-cli 49×2, m34 turn PASS, M4 16, M4.2 13,
M4.3 19, M4.4 8, m51 99/99 ×2, SSH 28, restore 51, a11y 3/0 fail with
the 4 standing accesskit-blocked skips.

**Probe gotchas recorded** (they cost three diagnostic rounds): the
app renders at the monitor's DPI scale (125% here) while an
un-tampered probe thread reads *logical* window sizes — so GetWindowRect
reported 914x598 for what is physically 1143x747, so sampling clipped
the top-left corner and hid the bottom 150 physical px (where the bar
and strip live). The probe thread must set
`SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2)`, read
`GetDpiForWindow` for the points->physical factor, and sample points×
scale. Second gotcha: the bar fill and the terminal viewport share the
`#0c0e14` token, so "above bar" checks cannot distinguish them; sample
fill rows that avoid the glyph band (the 12px mid-row text renders as
anti-aliased inks, never planar fills). Third: two tokens differ by
under 0x10 per channel (`#191b22` vs `#0c0e14`), so presence scans need
tolerance ≤6 or exact matching, not the ±14 used first.

**Stray-process policy refinement from the battery**: resident
`msedgewebview2.exe` processes on this machine belong to the user's
WhatsApp and Windows' BatteryWidgetHost — killing the whole process
name hits user state, the same way `msedge.exe` always did. cmux-owned
browser children are identified by ancestry (the WebView2 child of a
`cmux-shell.exe` or its cmux-owned children), and only those are
reaped.

## 10. Lane 2b landed: custom title bar (2026-10-24)

**Source of truth**: header node `1:495` "Header" in file
`AGKdhDGDEmQieIRKzc7ain` (session
`712daafa-d8a6-4836-b924-0d403c896bd3` exports the node spec). No value
was invented: the strip geometry, tokens, crumbs, and control glyphs
all come from that node (plus the design tokens of section 2).

**Spec as implemented**: 32pt full-width strip above the client, fill
`#191b22`, bottom 1px hairline `rgba(62,72,79,0.4)` over the strip
fill (blend ≈ rgb(40,45,52) — verified by pixel probe). Left cluster
pl=8: `cmux` 13px `#e2e2eb`, `/` 12px `#87929a`, then a chip
(`#1d1f26`, radius 4px, padding 4px, gap 4px) holding the workspace
name (≤140px, `#bdc8d1`) and `›` + pane name (≤100px, `#8ed5ff`);
untitled values render `–`. Right cluster: three 44×32 control zones
(minimize line 8.17×1.17pt, maximize square 9.75pt, close X 8.75pt
strokes, ~1pt ink `#bdc8d1`; hover fills `#27292E` and close-hover
`#C42B1C` with white ink). The whole strip carries an a11y hover label
"Title bar: cmux / {ws} › {pane}, drag to move window"; the three
buttons are individually labeled click widgets. One of the design
mock's sub-elements (INSPECTOR button, avatar, logo glyph — node
`1:524`) remains a recorded residual-skip: the port has no feature
behind them yet; node `1:512` (mid-row command bar) is lane 3.

**Verified facts that shaped the Win32 integration** (two earlier
beliefs were probe-falsified):
1. winit 0.30.13 frameless windows KEEP
   `WS_CAPTION|WS_SIZEBOX|WS_MAXIMIZEBOX|WS_MINIMIZEBOX`
   (window_state.rs `to_window_styles`) and drop the visible frame by
   returning 0 from `WM_NCCALCSIZE` — the subclass hook (a
   `GWLP_WNDPROC` subclass installed once) is therefore required for
   anything native-style.
2. FALSIFIED: "DefWindowProc keeps the resize borders alive for
   NCCALCSIZE=0 windows". With no nonclient area, DefWindowProc
   reports HTCLIENT for every interior point; the `HT_EDGE_*` codes
   never surface. The probe's edge drags did nothing before this was
   fixed (verified).
3. FALSIFIED: returning `HTCLIENT` instead of forwarding DID work for
   all buttons — the egui widgets are the only click targets in the
   strip, so the caption route is what the strip needs (verified by
   the drag/double-click-maximize probe steps).
4. The band computation had to move INTO the subclass (Chromium /
   WindowChrome recipe): `title_band_decision` computes
   `SM_CXSIZEFRAME + SM_CXPADDEDBORDER` (via `GetSystemMetricsForDpi`)
   bands on left/right/bottom/top (top band only outside the control
   zones; all bands suppressed while maximized). Band point→HT codes
   are unit-tested (`title_band_decision_matches_the_chromium_recipe`)
   and live-probe verified on both edges.
5. A WebView2 child pinned to the exact pane rect swallows the band
   (a child window answers HTCLIENT for every point it covers), so
   browser children are inset from window-adjacent edges by
   `resize_band_pts` in `sync_browser_surfaces` (verified by the
   right-edge drag over a browser pane now resizing).

**Two production bugs found and fixed inside the lane** (each
reproduced via `Start-Process -RedirectStandardError` +
`IsHungAppWindow` before the fix, healthy after):
- stack overflow: `install()` re-read `GWLP_WNDPROC` every frame and
  the stored original pointer got poisoned with the subclass itself →
  recursive `call_prev` at ~frame 2; fixed with a load-guard and a
  `compare_exchange` capture-once;
- infinite hang: `fit_crumb_galley` dropped one character but
  re-appended `…`, so the candidate length never shrank (busy loop
  while the main thread painted → `IsHungAppWindow=1`); fixed with a
  strictly-shrinking `count*3/4` cut. Both fixes unchanged.

**Live probes (kept as battery harness
`test_titlebar_2b.py`, visual + behavior modes, 13 checks each)**:
visual mode verifies the strip fill and corner, close-zone fill,
close X ink, maximize square, minimize line, bottom hairline, the
`cmux` ink, the workspace chip fill, and the below-strip layout
(13/13). Behavior mode exercises the maximize/restore via button,
caption double-click maximize/restore, right- and left-edge resize
(≥40px growth), minimize + external SW_RESTORE, and the close button
exiting the app (13/13). Both run at the monitor DPI (125% here) —
the probe thread must set
`SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2)`; right-anchored
control-zone clicks must track the CURRENT client width (maximized
client 1938px vs 1125px normal — stale W silently clicks outside
the zone).

**DPI probe gotchas worth keeping**: capture via
`PrintWindow(PW_RENDERFULLCONTENT)` then `GetDIBits` needs (a) the
bitmap deselected out of the DC before `GetDIBits`, and (b) a FULL
40-byte BITMAPINFOHEADER — a hand-rolled struct without
`biXPelsPerMeter`/`biYPelsPerMeter` is 32 bytes wide and,
passing `biSize=32`, makes `GetDIBits` fail silently with 0 scanned
lines (which reads as an all-black screenshot; both caught live).
The probe's second window is winit's "Winit Thread Event Target"
18×18 helper (pre-existing, untouched by the lane).

**Battery on the rebuilt binary** (untouched personalized config):
smoke 49×2 (first run had 5 timing flakes on browser-initializing
lanes, cleared on retry — same family as the m51 ledger), m34 turn
PASS, M4 16, M4.2 13, M4.3 19 (its selection drag anchored 12px below
the client top and slid the window via HTCAPTION — the test now
anchors on the first pane row below the strip), M4.4 8, m51 99 (the
child-pinned-rect assertion now expects the +32pt strip offset),
SSH 28, restore 51, a11y 3 pass with the 4 standing accesskit skips,
cargo test 327 (326 + the band-decision test), clippy/fmt clean.

## 11. Lane 3 landed: sub-header chips + command bar + palette (2026-10-24)

**Source of truth**: file `AGKdhDGDEmQieIRKzc7ain` nodes `1:5`
("Macbook Pro 14, subheader", session `4b46c6a5-910b-4232-b22d-ce1ab3cfd388`
exported the full spec), `1:7` (git chip), `1:14` (diff chip), `1:21`
(agent chip), `1:29` (right cluster), `1:54` (split tray), and `1:512`
("Command bar", toward row 13). All values below are design-node reads;
nothing invented. Glyphs were decoded from the node SVG exports into
procedural painters (same path-data fidelity rule as lane 2b).

**Spec as implemented** — 32pt sub-header band between the title strip
and the panes, fill `#191b22` (no bottom hairline: the design step is
the `#0c0e14` pane interior itself; the band starts at the sidebar's
right edge — the mock's full-width strip is a 1280px monolith, the port
keeps the aside independent). Chips sit `top+5`, 22pt tall, `left+8`,
8pt apart, radius 4:

- Git chip `1:7`: pad 4, fill `#0c0e14`, prompt glyph 10.833×8.667
  (`#8ed5ff` over the chip fill), abbreviated cwd (≤170pt, 12px
  `#e2e2eb`), then `6` gap + the `[branch*]` badge (12px `#ffc42f`).
  `abbreviate_home` renders the cwd under `%USERPROFILE%` as `~\…`.
  Hidden entirely while the sampler has no repository (the design has
  no repo-less variant — recorded residual).
- Diff chip `1:14`: pad 6, fill `#1d1f26`, Bold `+{adds}` `#8ed5ff`,
  Bold `-{dels}` `#ffb4ab`, then gap 2 + `staged` 11px `#87929a`;
  shown only when `staged_adds+staged_dels > 0`.
- Agent chip `1:21`: fill `rgba(79,49,156,0.2)` over the row (unmultiplied
  blend ≈ `#231f3a`), info dot + `CLAUDE CODE` (Bold 11px `#cebdff`)
  `·` `executing` (12px secondary). Gated by the REAL M3.4
  turn-boundary predicate (`active_agent_turns`, shared with the status
  bar) — idle panes never show it, where the mock paints it constant.
- Right cluster `1:29`/`1:54`: two keycap pills (fill `#1d1f26`,
  pad 4, 11px caps `#bdc8d1`, 10px `+` and 12px word `#87929a`,
  `pl=4`) then the tray (`#1d1f26` p2/gap2) of `#11131a` 18.5×20
  rounded buttons with glyph inks `#8ed5ff` (tray 1) and `#87929a`
  (tray 2, hover-bright). The pills render the EFFECTIVE Windows
  keymap (`splitRight` `ctrl+shift+r` → "v-split",
  `splitDown` `ctrl+shift+d` → "h-split" from `DEFAULT_BINDINGS`;
  an unbound action hides its pill). Tray buttons are real click
  widgets: `SplitFocused(Right)` and `EqualizeAll` (the latter was
  previously unreachable). The design's tray 3 (cross) and the Reset
  ⟳ pill stay recorded residual-skips — no port feature behind them.
- Command bar `1:512` (row 1, inside the title strip): bg `#11131a`,
  radius 4, px 8, 24pt tall at `y+4`; prompt glyph 11.667×9.333
  `#bdc8d1`; placeholder "Quick dispatch, processes or jump to pane..."
  12px `#bdc8d1`; right `Ctrl`/`K` pills `#1d1f26` px 4, 11px `#87929a`.
  Mock: 420×24 at x=425.68 of 1280 (0.3326). The port derives the bar
  purely from the client width (`title_command_bar_rect_pts`: left
  `max(0.3326·W, 335pt)`, width `min(420, W−148−left)`, hidden below
  800pt) so the wndproc hit-zone carve and the painter agree by
  construction — the carve lives in `strip_metrics` and drives both
  `title_bar_hit_decision` (HTCLIENT under the bar) and
  `title_band_decision` (no top resize band over the bar).

**Adaptations recorded** (no invented chrome): the git chip's data
source is a NEW async sampler over the PROCESS spawn cwd
(`cmux_shell::git_state::sample_git`: `rev-parse` + `status
--porcelain -b` + `diff --cached --numstat`, re-run every 2s on a
waking channel) — no OSC 7 pipeline yet, so a `cd` inside a pane does
not move the chip (documented limitation); branch `*` = dirty per the
porcelain parser. The design's 0.55px/letter tracking on the mock text
is below egui's glyph-render granularity — omitted. Chip verticals
center in the 32pt row (the mock centers in 24 with 8/10 paddings).
The command bar also paints a muted hover fill (the design nodes carry
no hover states at all).

**Row 13 (palette)**: the design file has no overlay node — the bar's
chrome extends into a modal: `#11131a` 420pt panel radius 4 px hairline,
anchored 8pt below the chrome; one query line ("▌" echo + blinking
caret) and 24pt rows (`selected` fill `#1d1f26`, label 12px primary,
right-aligned muted detail). Rows build on open: config `commands`
first (workspace-flavor entries skipped — the resolver refuses them),
then cross-workspace jump rows (`{pane title} — {workspace}`). The
palette is modal: `serve_palette` runs before `process_bindings`, and
while open consumes EVERY input event (a leaked key would type into a
pane). Enter activates, arrows wrap, typing filters (label + body,
case-insensitive), Esc closes. Command rows execute the M6
`command.run` semantics exactly (`resolve_config_command` +
`sanitize_command_text` + text+newline into the focused pane's
terminal); jump rows focus their pane (`pending_jump`, applied after
the drain). `Ctrl+K` is the DEFAULT_BINDINGS 20th row
(`commandPalette`); the bar click opens it too.

**Probe evidence (kept as battery harness `test_subheader_lane3.py`,
visual 17 checks + behavior 20 checks)**: visual mode verifies the row
fill, chip fills, info/amber/danger inks, agent-chip absence while
idle, pill/tray fills, and the command bar's fill/glyph/placeholder/
pill inks. Behavior mode proves: bar click and `Ctrl+K` open the
modal, Esc closes, arrows move the wrap selection, Enter runs the
selected config command into the focused terminal (read back via
`cmux read-screen`), the modal leaks no typing, and the tray's
v-split button splits the pane (gutter verified + surface count).
Two probe lessons: (a) `#11131a` vs `#0c0e14` differ only by 6/channel
— fill assertions must stay at tol ≤4 or they become tolerance-dupes;
(b) a glyph's ink pixels cut a button's fill run short, so click
targets must locate the FIRST run ≥4px, not the widest run — the
initial pattern silently clicked EqualizeAll.

**Legacy flake notes**: m43's first-call timeout abort cleared on
retry (ledger); smoke-cli's redirected native stderr aborted the run
at the documented identify/browser-init retry sites — the two retry
loops now scope `$ErrorActionPreference='Continue'` so the diagnostic
prints without aborting. m43's drag anchor and m51's pane-top now
expect the 64pt chrome (+32 sub-header). Battery on the rebuilt
binary: smoke 49×2, m34 PASS, M4 16, M4.2 13, M4.3 19, M4.4 8, m51 99,
SSH 28, restore 51, a11y 3 pass/4 standing skips/0 fail, lane-3 probe
17+20, workspace `cargo test` 324, clippy/fmt clean.

## 12. Lane 4 landed: aside runtime-services segment (2026-10-24)

**Source of truth**: file `AGKdhDGDEmQieIRKzc7ain` aside node `1:399`
(256x992) and the services block as a `Margin` child, node `1:456`
("RUNTIME SERVICES", session `cfc26fdc-9f9f-4082-b87b-a477f24df490`).
The spec was pinned with `get_design_context` (design-to-code skill
declared, screenshot requested) plus a saved metadata dump of the aside
tree: the services Margin sits at `y164.33`, exactly the bottom of the
mock's workspace rows (28px tall, bottom `164.33`), and carries an
internal +12 gap from its own frame to the header text. All values
below are design-node reads; nothing invented.

**Spec as implemented** - segment after the workspace rows, header
always drawn (an empty segment is header only): header "RUNTIME
SERVICES" Inter Bold 11px, 0.55px/letter tracking, uppercase, ink
`#87929a`, line height 14px, at `rect.left+12`; rows 26pt tall, fill
`#0c0e14`, radius 4, padding 4, 2pt gaps, width aside-16 (the port
draws the established +/-6 sidebar band, x `left+6..right-6`); port
text 12px/18px `:port` in `#ffdf9f` (warm) when UP or `#87929a` (muted)
when STOPped; service name 12px/16px `#87929a`; status badge Bold 11px
`#8ed5ff` (UP) / `#87929a` (STOP), right-aligned `right-4-width`.
In the port's geometry (sidebar rect below the 32pt strip): workspace
rows 72..102, header 114..128, rows top 132, row1 132..158, row2
160..186.

**Data plane**: a "cmux-service-watcher" thread samples every 2s
(`SERVICE_SAMPLE_INTERVAL_MS`): for EVERY workspace's pane roots (the
sampler sends all roots; the sidebar filters to the active workspace)
it snapshots the owning process tree pid (captured at shell handoff as
`Pane.tree_pid` from `ShellHandoff.tree_pid`) and the OS TCP listener
table (GetTcpTable2/GetTcp6Table2 windows-rs 0.62.2: IpHelper +
WinSock + ToolHelp + Foundation features). A listener row = `(port,
label)`: `:port` in warm ink, badge UP; the label is the owning process
image base name lowercased minus `.exe` (the design's `web-vite`-style
names are mock illustration, not a spec source).

**Adaptations recorded** (no invented chrome): the design's serif/
Liberation faces port to the Proportional family (lane-2b precedent);
the 0.55px tracking stays a recorded egui galley residual; STOP is a
latched last-seen label kept while the pane root lives, keyed
`(workspace_idx, root pid, port)`, so a closed pane prunes its history
even with a sibling pane serving; rows cap at `MAX_SERVICE_ROWS = 8`
(bounds history growth); after a workspace-index shift rows may lag
one 2s cadence; a tick with a failed snapshot sends nothing (never
invents state) and the worker logs failure once + recovery once.

**Probe evidence (kept as battery harness
`test_sidebar_services_lane4.py`, 24 visual checks)**: launch empty
(header ink present, no row fill/warm/badges), seed the real listener
through the pane's own shell (`command.run` + netstat pid as
cross-evidence), UP row (first-fill-run width, warm `:port` ink, blue
UP badge, row fill pixels), kill the owner pid (taskkill) and observe
the STOP latch (fill persists with the row drawn in muted ink, warm
and badge gone), and the pane exit (`exit` + CR typed via `cmux send`)
closing the last workspace's window (the map-level prune behind it is
unit-tested as `dead_root_leaves_no_history_row`).

**Probe lessons**: (a) the sidebar rect starts BELOW the 32pt title
strip - the first probe table assumed `rect.top = 0` and read header
ink 0; a live ink profile (per-line ink counts over the sidebar column)
placed the real bands 32pt lower (workspace text mid 87pt, header
116.8..125.6pt, warm numerals 140.8..147.2pt) and the corrected table
turned 10/3 into 24/24; (b) the seed exposed a REAL M6 defect now
fixed: config commands typed `command + "\n"` (upstream's
`CmuxConfigExecutor` 196 byte-faithful) but a raw LF is not Enter for
pwsh's PSReadLine - the command sat on a `>>` continuation prompt and
the smoke's marker check had been passing on the visible-but-unexecuted
text; the port types `command + "\r"` now (`command_input_bytes`,
CR = the Windows Enter, submits in pwsh and cmd).

## 13. Lane 5 landed: agent activity stream pane (2026-10-26)

**Source of truth**: file `AGKdhDGDEmQieIRKzc7ain` agent pane node
`1:244` with its stream body `1:257` (the completed card node `1:258`,
the active card node `1:299`, the pane's tab node `1:245` at 19.67pt
tall). The spec was pinned with `get_design_context` (design-to-code
skill declared, screenshot requested) during implementation; the values
below are design-node reads, nothing invented. Upstream's CLI has no
agent verb - the pane is the design file's agent surface, recorded as
a port extension (rows 8/9/10 carry the behavior; this section carries
the visuals).

**Spec as implemented** - the pane body is the agent's in-app activity
feed, split right of a surface (`agent.open_split` wire verb +
`cmux agent [--surface <spec>] open [--json]` CLI):
- Tab row (node 1:245): fill `#282a31`, 20pt tall in the port (design
  19.67), the agent glyph hand-drawn in ink `#e2e2eb` + the title
  "Agent activity" 12px `#e2e2eb`; while a turn boundary is pending, a
  warm chip rides right-aligned 4pt from the tab's right edge - fill
  `#605022`, radius 4, 14pt tall, 11px accent ink `#ffc42f`, text = the
  agent name uppercased.
- Stream body (node 1:257): fill `#0c0e14`, 8pt inset on all sides,
  bottom-anchored card stack, 8pt card gaps, radius-4 cards; the
  scroll offset accumulates upward and is clamped against the content
  before painting (bottom-anchored viewport).
- Completed card (node 1:258): fill `#191b22`, 6pt padding, a
  circle-check glyph hand-drawn in the muted ink inside the 12.5x14.5
  icon slot, headline 12px `#e2e2eb` at the 18pt leading, the event's
  real elapsed time right-aligned in 11px `#87929a` (`{sec:.2}s`), and
  the detail body 12px `#bdc8d1` wrapped to the column.
- Active card (node 1:299): fill `#1d1f26` with a 1pt `#ffc42f` accent
  ring, a 4pt accent dot + the agent label 11px accent uppercased,
  "RUNNING" 12px accent right-aligned, a dark `#0c0e14` block carrying
  the frozen terminal tail rows (monospace 12px `#e2e2eb`, 18pt row
  leading), and the real pending age right-aligned at the footer in
  12px `#87929a`.

**Data plane**: a pure `cmux-shell::agent_cards` model (9 unit tests) -
`CardStore` keeps one bounded `VecDeque` per workspace
(`CARDS_MAX_PER_WORKSPACE = 24`, oldest evicted; `sync_len` mirrors
workspace close/order shifts exactly like the lane-4 services cadence;
a stale workspace index push is a no-op; the workspace close clears its
slot). The stream renders REAL ingest paths only: (a) OSC 9/777 hook
notifications (title verbatim; OSC 9's missing title lifts the body's
first line into the headline and drops that line from the detail);
(b) prompt-turn deliveries, holding upstream's handler text verbatim -
the agent display name as the headline + "Task completed" as the body;
(c) every validated hook-pipe event, journal routes included (title
`cmux <subcommand>`, body = the payload `message` up to 160 chars) -
journal routes (`feed`, `pre-tool-use`) become cards only and still
never ring or toast. The active cards come from the live M3.4
turn-boundary facts (arrival = `deadline - delay`, both stored; the
screen tail froze into the boundary when it was seen, so the paint pass
is a read-only walk that never touches the terminal).

**Adaptations recorded** (no invented chrome): the design's Figma faces
port to the Proportional/Monospace stack (lane-2b precedent); letter
tracking stays a recorded egui galley residual (lanes 2-4 precedent);
the agent and circle-check glyphs are hand-drawn strokes (the design's
glyph art carries no font); each card's height is measured from
laid-out text so wrapped bodies size the card.
- Residuals recorded, not invented: the design's prompt footer (the
  lane-5b follow-up), the diff-preview card, and the
  gate/confirm-approval card - the stream renders only card kinds the
  port's data plane actually produces.
- Divergence: `agent.open_split`'s wire reply reuses the browser
  open-split reply shape (`surface_id`/`pane_id`/`workspace_id` +
  `placement: "split"`) and adds `"type": "agent"`; upstream has no
  agent verb, so there is no upstream reply text to mirror.
- Persistence divergence (truthfulness): agent leaves save
  `panel_type: "agent"` (no browser URL); restore rebuilds them via the
  agent constructor with no process to spawn and an EMPTY stream -
  cards are not persisted, so the restored snapshot stays truthful
  about what it backs.
- The stream is read-only: a click focuses the pane (a11y label
  "Agent activity"), the wheel pans the bottom-anchored viewport, and
  `surface.send_text`/`surface.read_text` refuse agent surfaces
  (the `has no terminal` not-found path).

**Probe evidence (kept as battery harness
`test_agent_activity_lane5.py`, 36 checks)**: agent pane opened through
the CLI (reply shape + `system.identify`/`surface.list` `agent`
labels), tab fill `#282a31` + stream fill `#0c0e14` + empty-stream
baseline, hook-seeded cards grow (fill `#191b22`, headline `#e2e2eb`,
time ink `#87929a`; `feed` as a journal route with no ring/toast), the
full m34 turn boundary through the pane's own shell (accent `#ffc42f`
ring while pending - ring gone at delivery - completed card), close
semantics (the agent pane keeps the workspace alive after the seeded
terminal pane exits and refuses text input; the plain-mode grammar is
asserted here where a second uncloseable pane no longer disturbs
counting), and a relaunch round trip (the agent pane restores with the
original id, its tab paints, the stream stays empty). 36/36 twice on
the clean rebuilt binary.

**Probe lessons**: (a) the pixel band must be anchored to LIVE-measured
app geometry (an instrumented paint print, since removed): agent leaf
`[[564,64]..[900,536]]`pt at the default window (right split about 37%
of client width), chrome bands = title strip 0-32pt + the M2.4b
pane-tab strip 32-64pt, the pane's own tab 64-84pt, stream fill from
about 90pt, leaf bottom 24pt above the client bottom - the probe band
is x0=0.64·cwidth, x1=-4, y0=90pt, y1=cheight-24·s-2 and its tab run
scans 66-84pt; outside it the app's near-token chrome inks
(`#191b22`-class fills) produce false card baselines. (b) the m34
hand-over must be ONE python invocation typed into the pane's pwsh
(`cmux send` the text, one `"\r"` submits): arm the prompt (Ollama
line + CRLF), then `>>> Return FINAL_OLLAMA_OK` + the marker, then the
bare prompt line held while the process sleeps about 11s - that opens
the about-500ms pending window; two separately typed commands leave
pwsh typed-line echo noise and never fire. (c) the delivered-card
assert needs `n3 > n2 + 2` for the +/-1 sampling noise across the
window. (d) `cmux hooks enqueue claude <sub>` needs `CMUX_SURFACE_ID`
in the CLI subprocess env (pinning the workspace) and reads the
payload from stdin; the CLI fails open (exit 0, `{}`) on anything bad,
so assertions run on pixels, not CLI output. (e) a background
launch/SW_RESTORE leaves a full-screen maximized foreground overlay
eating every injected click - the titlebar-2b probe gained
`pin_foreground` (ALT-trick + retry) at window discovery and again
after restore-from-minimize; the app's own close path verified clean
in a standalone diagnostic (clean exit, session snapshot saved).

## 14. Design-fidelity round 1 corrections (2026-10-26)

The owner reported the render "not identical — the color, the
responsive". Root cause (live captures + a read of
`C:\Users\noah\.config\cmux\cmux.json`): that file carries the legacy
"Apple System Colors" theme block (`background #1e1e1e`), and the
port's theme precedence (ADR-0008 divergence 7) made only SOME surfaces
follow it — title strip, sidebar, gutters and the pane interior —
while the sub-header band, status-bar fill, product chip, command bar
and every chrome chip/pill/row painted raw design tokens. The result
was chrome split between two schemes (live: title `#1a1a1a` under
sub-header `#191b22`, status bar `#0c0e14`, chips `#1d1f26`, interior
`#1e1e1e`). Two further divergences showed in the same capture: the
aside painted 216pt wide against design node `1:399`'s 256, and
Windows PowerShell 5.1 titles its console with the absolute image
path ("- C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
which overflowed the breadcrumb chip, the sidebar row, and the
status-bar marks (the last collided with the right cluster).

**Corrections, all coded in `port/crates/cmux-shell/src/main.rs`**:
- Theme-ladder coherence: every chrome fill now paints through
  `ThemeRuntime` — `background`/`sidebar_background`/
  `viewport_background` + a NEW `chip_background` (the raw `TOKEN_*`
  sites never consulted the theme). 19 fill sites routed (5 viewport,
  3 main, 2 sidebar, 9 chip). `chip_background` is `#1d1f26` by
  default and derives `shade_lighter(sidebar)` when a background is
  configured — the design ladder `#191b22 / 0.88` rounds to `#1c1f27`,
  within one channel of the flat design chip token: recorded
  approximation, not a design read (new `shade_lighter`, the inverse
  of `shade_darker`, channels rounded + clamped).
- Pane-INTERIOR faces (the agent lane's card fills `#191b22`/
  `#1d1f26`/`#0c0e14` and tab `#282a31`) deliberately stay the raw
  design tokens: they are content, not chrome, and the lane-5 spec
  pins them. A configured theme recolors the window, not the card
  faces.
- `SIDEBAR_WIDTH` 216 → 256 (the design width of node `1:399`).
- Friendly OSC-title fold (`friendly_osc_title` + `display_title`):
  a title that parses as an absolute drive/UNC path whose last segment
  ends in `.exe` (case-insensitive) folds to the image stem; app-known
  titles (`title_override`) never touch the fold. The port's title
  consumers now read `powershell` where the raw path used to flow in —
  the same OS behavior Windows Terminal shows, folded for the port's
  compact chrome.
- Status-bar marker budget (`draw_status_bar`): the right cluster is
  measured FIRST (it owns the fixed right edge), the CPU/MEM/agent
  telemetry is reserved before any marker is laid out, marker titles
  cap at 14 codepoints (`STATUS_MARKER_MAX_TITLE_CHARS`, fold
  ellipsis), and overflow markers drop behind a muted ellipsis. The
  telemetry segments stay visible in every width (hard-reserved design
  telemetry).
- Status-bar IPC leaf fold: the port's default control pipe derives
  from the machine SID (`cmux-s-1-5-21-1806527352-…`, about 50 chars)
  and overflowed its segment exactly like the titles did — leaves over
  14 codepoints fold to a leading ellipsis + the last 13
  (`status_ipc_label`, `pipe: ` prefix outside the cap).

**Live verification** (owner's config in effect, 1125x700 client,
dpi 120 — after-capture `cmux_live_3.png` beside the pre-fix
`cmux_live_1.png` in `%TEMP%\opencode`): title strip, sub-header,
sidebar and gutters `#1a1a1a` (the shade ladder), pane interior,
status bar and chips `#1e1e1e`, ring `#38bdf8` at the pane edge
x=320px (= 256pt), breadcrumb `cmux / powershell › powershell`,
sidebar row `powershell`, status bar
`1:powershell* | CPU: 30% MEM: 11 GB CLAUDE: IDLE` + folded
`pipe: …09267108-1001` + `UTF-8` + the `cmux-win11` chip — zero
collisions, zero drops at the default size. With no `theme` block
configured, every default remains the exact Figma palette (the
unconfigured ladder is untouched; unit-pinned in the theme tests).
Owner decision recorded open: their `cmux.json`'s legacy theme block
is their file and was NOT modified — dropping it would put the
producer on the exact design palette; the config contract itself stays
(config wins), never the reverse.

## 15. Design-fidelity round 2 corrections (2026-10-26)

The owner's directive "do as figma" plus two aside bug reports ("there
is a wierd sympole to close the tab for open workspace so please sub
this with just normal x symbol and there is no + sign to add new
workspace - add it"). Two verified root causes, both fixed in
`port/crates/cmux-shell/src/main.rs`:

- **Sub-header overrun covered the + row.** The strip (node 1:5) was
  painted from the WINDOW edge even though its own comment already
  said "the sub-header spans the main area only"; the design aside
  keeps its full height under the 32px title row (node `1:399`: "full
  height minus 32px title row", verified reads above). Its `#191b22`
  fill therefore rode over the aside's top band and covered the `+`
  (new workspace) row that `draw_sidebar` paints first (glyph at pt
  (128,53); pixel scan of the pre-fix capture `cmux_live_3.png` found
  ZERO ink in the band y 44..90 while the code matched — the row
  painted, the fill covered it). Fix: compute `sidebar_rect` BEFORE
  `subheader_rect` and start the strip at `sidebar_rect.right()`.
  The + was never missing from the code (verified read of
  `draw_sidebar`) — the overlap was the sole cause of the owner's
  "there is no +" report.
- **The close glyph source literal carried mojibake bytes.** A python
  byte audit found the row's painted literal at `main.rs` line 8787
  carrying `C3 83 E2 80 94` ("Ã" + em-dash — a double-encoded `×`);
  the app really painted "Ã—". Earlier greps that "showed" a correct
  U+00D7 were confounded by the Windows console's cp1252 re-encoding
  of tool output: byte-level audits are the ground truth for this
  codebase. Replaced with a hand-drawn x — two `painter.line_segment`
  strokes (arm 4.5, 1.0 stroke, muted ink, primary on hover) — no
  font or encoding dependency, satisfying "just normal x symbol".

**Theme block drop (owner-authorized):** the legacy "Apple System
Colors" block (`background #1e1e1e`, sec. 14) was REMOVED from the
owner's personal `C:\Users\noah\.config\cmux\cmux.json` after the
owner said "do as figma" — the file is otherwise byte-identical
(appearance, panes, terminal, shortcuts preserved). The config-wins
ladder contract itself stays (a future config could still recolor
chrome — that is the feature, not a regression): unconfigured now
shows the exact Figma palette, live-verified below.

**Mojibake inventory (recorded, NOT fixed this round):** a
comment-layer mojibake family exists across `main.rs` (about 90
sites: U+00C3+U+2014 and U+00E2-based em-dash/quote/arrow/ellipsis
encodings inside doc comments and section banners, e.g. lines
76-77, 1545, 6568-6569, 8984). All are valid UTF-8, stray glyphs
only inside comments, zero UI strings — cosmetic debt for a later
normalization pass. Byte-audit harness at
`%TEMP%\opencode\ascii_audit.py` (temp, owner disposal).

**Live verification** (round-2 binary, plain relaunch with the owner
env; after-capture `cmux_live_4.png` vs pre-fix `cmux_live_3.png`
under `%TEMP%\opencode`; 1125x700 client, dpi 120):
- Palette, pixel-read: sidebar `#191b22`, chips `#1d1f26`, title
  strip + command bar `#11131a`, pane interior + status-bar fill
  `#0c0e14`, focus ring `#38bdf8` at x=320px (the 256pt aside edge),
  sub-header fill `#191b22` (matches section 2's verified design
  read). Every chrome token is design-exact.
- The `+` row: visible ink 18 px at x 155..163, y 65..73 (pt center
  (128,53)).
- The close glyph: two stroke runs, 24 ink px (y 104..115 in the
  first row's close zone) — no A-tilde/em-dash pair.
- Lane-3 left-chip probe scans re-anchored to the sidebar edge
  (`round(256·s)` start, 0.72/0.80·cwidth windows) since the
  sub-header moved right. Full battery green on this binary, first
  runs, zero flakes: titlebar-2b visual 13/13 + behavior 13/13,
  lane-3 17/17, lane-4 24/24 twice, m43 19/19, lane-5 36/36,
  m51 99/99.
- **New behavioral proof** (`plus_click_probe.py`, temp, owner
  disposal): fixture-env instance; the 2b clip recipe works only when
  the foreground pin is VERIFIED (retry loop until
  `GetForegroundWindow` sticks) + a `ClientToScreen` coordinate
  mapping + a `WindowFromPoint` app-owner guard; real mouse down/up
  at the + row center (314,220): IPC `workspace.list` before=1
  after=2 (`w2` created and selected), then graceful WM_CLOSE
  teardown. The failed early attempts (SendInput and PostMessageW,
  both no-op) traced to the probe's UNVERIFIED pin — same-binary 2b
  behavior 13/13 proves click delivery itself is sound. Lesson:
  injected-click probes must verify the pin like 2b does.

Operator-slip disclosure: once this round the previous app instance
was hard-killed by mistake (a `Stop-Process` in a chain executed
before a later syntax error aborted the chain). The clean WM_CLOSE
rule was re-applied immediately; the next launch restored the pane
path. Incident closed; the clean-exit rule stands.

## 16. Owner-directed status-bar adjustments (2026-10-26)

Two owner requests for the bottom furniture, both deliberate
divergences from the mock (recorded, owner-directed):

- **Smaller telemetry font** ("make the status that tell the cpu and
  memory the font is smaller so it never collapse"): the left
  cluster's `|` + CPU/MEM + agent-state run now paints 10px
  (`telemetry_font` in `draw_status_bar`) where the pane markers keep
  the 12px left font. The run is measured with the same font it
  paints, so the round-1 hard-reserve construction (sec. 14) simply
  reserves a narrower span — the telemetry cannot collide with the
  marker run by construction and overflow markers still fold first.
  Verified live on the round-3 binary: the identical `CLAUDE: IDLE`
  block reads 9px tall (was 11px at 12pt), the whole telemetry run
  ends 43px earlier, and the pane-marker/right-cluster zones are
  unchanged.
- **Version display removed** ("the thing that tell the version
  remove it"): the aside's bottom strip (design node 1:485
  "Background+HorizontalBorder" = chip fill + top hairline + the
  version chip `#0c0e14` with the info-blue dot and the
  `v` + CARGO_PKG_VERSION label; its settings/help icon links were
  already recorded residuals) is no longer drawn, and its 38pt
  layout budget (`SIDEBAR_BOTTOM_STRIP`) was released — workspace
  and RUNTIME SERVICES rows now bound 6pt above the aside bottom
  (matching the rows' own 6pt side inset). `status_version_label`
  and its unit pin left with it. The status bar's right-edge
  `cmux-win11` product chip is NOT a version display (it is the
  design's product tag) and stays; flagged to the owner in case
  their ask included it.

Pixel method (round-3 build, `cmux_live_5.png` vs `cmux_live_4.png`
under `%TEMP%\opencode`, 1125x700 client dpi 120): the aside-bottom
band paints plain sidebar `#191b22` with ZERO ink pixels where the
pre-fix band was chip-fill `#1d1f26` with 180 ink px (the strip's
chip); the UTF-8/product-chip zones measure identical before/after.
No harness probe sampled the removed band or the telemetry glyphs
(grep audit before the change). Full battery re-ran on the round-3
binary, first runs, zero flakes: titlebar-2b visual 13/13 +
behavior 13/13, lane-3 17/17, lane-4 24/24, m43 19/19, lane-5
36/36, m51 99/99.

## 17. Owner-directed sub-header removal (2026-10-26)

- **The ask** ("the bar in that tell the instruction that has the
  short cuts remove it too please") removes the ENTIRE lane 3
  sub-header band (`Minimal Top Sub-Header`, node 1:5): the git chip,
  the staged-diff chip, the agent-state chip, the split-shortcut
  keycap pills (Ctrl+Shift+R / Ctrl+Shift+D), and the split tray
  (v-split + equalize buttons). Sec. 11 above now doubles as the
  restoration spec for that design; this section is the removal
  record. The command bar and palette (row 13) stay — they open from
  the title bar and the `commandPalette` keymap stroke (`ctrl+k`),
  which never depended on the band.
- **Layout**: `subheader_rect` and `SUB_HEADER_HEIGHT` are gone;
  `main_rect` starts at the title row's bottom edge (32pt) again, so
  the panes (and the WebView2/webview children) sit directly under the
  title strip. The palette overlay's anchor dropped from
  `title+sub-header+8` to `title+8` (40pt), matching the new chrome.
- **Removed with the band** (dead private code, restoration note at
  each seam): the git sampler fields + `refresh_git_sample` pump +
  `GIT_SAMPLE_INTERVAL_MS` + the `cmux-git-watcher` spawn block (the
  `cmux_shell::git_state` PARSING LIB stays public with its unit
  tests — no dead-code risk); the keycap helpers `humanize_key` +
  `keycap_tokens` (plus their unit pin); the tray glyph painters
  `paint_vsplit_glyph` + `paint_equalize_glyph`; the agent-chip violet
  pair `TOKEN_AGENT_INK` (0xCEBDFF over the chrome `#191b22`) and
  `agent_chip_bg` (rgba(79,49,156,0.2)).
- **Affordance consequences (disclosed, deliberate)**: the split tray
  buttons and the keycap pills were the band's only affordances —
  splitting lives on through the keymap's owner-accepted defaults
  (`ctrl+shift+r` / `ctrl+shift+d`, probe-verified in the rewritten
  lane-3 behavior half); `equalizeSplits` stays a valid config id
  (unbound by default — unchanged pre-round 4) but has NO clickable
  surface anymore; the palette has no builtin equalize row (only
  config commands + jump targets). The git/diff/agent data displays
  are gone from the UI entirely; nothing else carries them.
- **Restoration appendix** — exact bodies as cut (round 4):
  - `TOKEN_AGENT_INK: u32 = 0xCEBDFF` and
    `fn agent_chip_bg() -> egui::Color32 { egui::Color32::
    from_rgba_unmultiplied(0x4F, 0x31, 0x9C, 51) }`
  - `const SUB_HEADER_HEIGHT: f32 = 32.0;`
  - `const GIT_SAMPLE_INTERVAL_MS: u128 = 2_000;`
  - `fn paint_vsplit_glyph(painter: &egui::Painter, center: egui::Pos2,
    ink: egui::Color32)` — two rect bars 3.5x10.5 at x offsets 0.0/7.0
    from center-5.25, corner radius 1.
  - `fn paint_equalize_glyph(...)` — stroke 1.166; half 5.25; arm 2.92;
    four corner brackets: per (±1,±1) corner tip at center+half, arms
    pulled back 2.92 along x and y.
  - `fn keycap_tokens(combo: KeyCombo) -> Vec<String>` — push "Ctrl"/
    "Shift"/"Alt" per combo.mods, then `humanize_key(combo.key)`.
  - `refresh_git_sample` — cadence check (`git_requested_at` vs
    `GIT_SAMPLE_INTERVAL_MS`), `git_requests.send(git_cwd.clone())`,
    drain `git_results` into `git_meta`.
  - watcher spawn: `mpsc::channel::<PathBuf>()` pair +
    `mpsc::channel::<Option<cmux_shell::git_state::GitMeta>>()`
    carrying `Option<GitMeta>` (failed rev-parse still completes a
    round trip); thread "cmux-git-watcher" loops `git_request_rx.recv()`
    -> `sample_git(&cwd)` -> send -> `wake.wake_now()`.
  - `humanize_key` — doc: "(`R`, `Tab`, `[`)"-shaped key names; body
    was a `match key` over Tab/Enter/Space/.../backtick to owned
    strings (the full 60-line table was not byte-preserved pre-cut;
    re-derivable from the KEPT label->Key parser, which mirrors it).
- **Verification** (round-4 binary, owner env): gates fmt 0, clippy
  `-D warnings` 0, workspace tests 369/0 (minus 1: the keycap unit
  pin), build clean on the gracefully closed app. Pixel diff
  `cmux_live_5.png` vs `cmux_live_6.png`: exact `#191b22` band-fill
  px 12444 -> 0 in y 41..63 right of the aside; ring top-edge accent
  px 0 -> 740 (the focused pane's #38BDF8 ring now rides the pane top
  edge at the title strip's bottom line); title strip + status-bar
  right cluster zones identical.
- **Probe record**: test_subheader_lane3.py rewritten as the band
  ABSENCE probe (15 visual + 22 behavior assertions; absence uses
  exact-match colors — the diag uncovered ±4-tolerance false hits
  from prompt-glyph AA at #1c1e24/#1d1f24, never the removed fills).
  The behavior trio's deterministic bar-click failures were the
  probe's OWN unverified foreground pin (WindowsTerminal sat over the
  probe instance; `WindowFromPoint` hit pid-16852's window) — slated
  the m34/2b ALT-trick `pin_foreground` recipe into the probe
  verbatim, fixing it app-untouched (Ctrl+K palette + keymap split
  were green throughout). lane-5's `agent_band`/`tab_run` re-anchored
  (leaf top 64pt -> 32pt: `90*s`->`58*s`, `66..84*s`->`34..52*s`) —
  the pre-edit anchor audit had missed non-standard forms. Battery on
  the round-4 binary: titlebar-2b 13/13 + 13/13, lane-3 15/15 +
  22/22, lane-4 24/24, m43 19/19, lane-5 36/36, m51 99/99.

## 18. Design-fidelity round 5: final side-by-side reconciliation (2026-10-26)

The owner's directive - "finish all the design rounds" - closed with the
parked Figma side-by-side. Honest scope: the Figma MCP session opened
(`whoami`: Starter tier, View seat; `create_session` returned
`413d7bf0-0a83-424b-b8a6-eac553111327`), but every file read stays behind
this month's plan paywall (`get_metadata` answers the rate-limit banner),
so the comparison runs against secs 1-17 - the verified node reads
recorded in 2026-10-22/23 that the round-1..4 lanes already
pixel-validated. Build side: one fresh live capture, `cmux_live_7.png`
(plain relaunch, no env overrides; restored session; 1125x700 client,
dpi 120). No code changed in this round - the scan is a one-shot
analysis (scripts in `%TEMP%\opencode`, owner disposal:
`sidebyside_final_r5.py`, `diag_r5*.py`).

**Predicate lesson worth keeping:** the ring's horizontal edges can NOT
be verified by exact `#38bdf8` at this DPI. The 1.25 scale straddles the
pixel grid: the top edge (y=41) and bottom edge (y=669) plus the right
column (x=1124) paint subpixel-AA cyan-family pixels (RGB predicate
r<130, 120<g<230, b>195 - the RGB form of the battery's BGR rule
B>200/120<G<220/R<120), while the left column (x=320) lands on exact
`#38bdf8`. Exact-only scans at those edges read false absence; the
battery predicate was already correct (this round's 797 family px on
y41 vs the round-4 diff's 740 is the same rule).

**Per-surface verdicts (design value against live measurement,
cmux_live_7.png):**

| Surface (design source) | Design value | Live measurement | Verdict |
| --- | --- | --- | --- |
| Title strip (1:495) | fill `#191b22`, 32pt, hairline rgba(62,72,79,0.4) = blend rgb(40,45,52) | spare-band fill share 1.000; hairline row y=39: 450/450 px at tol 4 | MATCH |
| Crumb chip | `#1d1f26` in strip | 10.6k exact px, bbox y 7..34 | MATCH |
| Command bar (1:512) | `#11131a`, 24pt at y+4, left max(0.3326*W, 335pt) | rect x 419..940, y 5..35 px at the 900pt client; fill 1018 px top rows; pills `#1d1f26` 613 px; placeholder ink 897 px | MATCH |
| Aside (1:399) | 256pt wide, `#191b22` | 320px; fill share 0.918 of the sidebar zone | MATCH |
| Workspace rows | active chip `#1d1f26` (sec 14 ladder) | row-1 chip at y 91..127 = the sec-12 geometry rows 72..102pt; + row ink 35 px; first-row close-x 590 px; active row name info family 245 px | MATCH |
| Focus ring (sec 2) | 1px `#38bdf8`, radius 4 | top edge y41 797 family px (x 324..1120), left column 610 exact px, bottom edge closes at y669, one family row above the status bar | MATCH |
| Main area | gutters `#11131a`, interior `#0c0e14` | interior share 1.000 in the pane body; the restored single-pane session shows no gutter surface (split-gutter sheets stay covered by m43/lane-5, probe-green) | MATCH |
| Status bar (1:371) | fill `#0c0e14`, right cluster 11px, primary-ink product chip on `#191b22` | fill dominant share 0.758 (rest = text AA + chip); chip bbox x 1028..1112 y 676..693, 1166 fill px + 1 coincidental text-AA px at x=545 (cosmetic); chip ink primary-family 152 px; UTF-8 block muted ink 515 px | MATCH |
| Pane markers | focused marker info-blue | left-cluster info family 138 px (byte-identical to live6) | MATCH |
| RUNTIME SERVICES (1:456) | header + data-gated rows | restored session holds no live listeners: 0 warm, 0 badge px (header-only segment); the seeded-listener case stays verified by lane-4 24/24 | MATCH (data-gated) |

**Owner divergences (EXPECTED, all recorded above):** the sub-header
band absent (y 42..65: zero `#191b22`/`#1d1f26` exact px; sec 17), the
aside version strip absent (zero `#1d1f26` in the bottom 48px of the
aside; sec 16), the `cmux-win11` status chip kept (sec 16; owner never
answered the repeated question - still open). Telemetry re-measured at
10px: the left-cluster ink band stands 14 rows tall (y 670..691).

**live6 -> live7 drift attribution (23954 diff px, all session state,
zero renderer drift):** aside 23734 px in exactly two chip-height bands
(y 90..128: 11864 px; y 170..208: 11870 px) - live6 was captured under
the round-4 session whose active workspace row sat lower (its chip
painted there), live7 restored with row 1 active; row pitch 40px and
the row texts identical. Status bar 220 px = CPU/MEM digits + telemetry
tail at x 480..550. Title strip 0 px, terminal zone 0 px.

**Residual reconciliation (grepped `residual` across `main.rs` +
docs, 2026-10-26):**

| Residual | Status |
| --- | --- |
| Version-strip settings/help icon links (sec 9) | MOOT with the owner's round-3 strip removal; the sec-11 restoration spec keeps them if the strip ever returns |
| Address glyph before the listener segment (sec 9; `draw_status_bar` doc) | STANDING - no icon font, no destination |
| INSPECTOR / avatar (1:524) and the 16px logo glyph (1:497) (sec 10; `draw_title_bar` doc) | STANDING - no port feature behind them; logo explicitly skipped |
| Git chip repo-less variant (sec 11) | MOOT with the owner's round-4 band removal (the git chip left with the band; the restoration spec keeps the variant question) |
| Tray 3 cross + Reset pill (sec 11) | MOOT with the same band removal (sec 17) |
| 0.55px/letter tracking (secs 12/13: services header, agent cards) | STANDING - egui galley limitation |
| Agent prompt footer / diff-preview card / approval gate (sec 13; `paint_agent_stream` doc) | STANDING - rows 8/9/10 carry the behavior; no data plane for those card kinds yet |
| Screen variants beyond the one mock (sec 7) | STANDING file-level - no light variant in the file; a configured light scheme keeps egui defaults (`install_design_visuals`) |
| Palette fuzzy-search adaptation | the recorded adaptation stands: case-insensitive substring filter (the design has no overlay node; sec 11) |

Design rounds are complete: lanes 1, 2, 2b, 3 (removed by owner
direction), 4, 5 plus the owner divergence rounds (secs 14-17) all land,
verified, and this reconciliation is the closing record. Remaining open
product question at the time: the `cmux-win11` chip (kept; the owner
had not yet answered whether their ask included it) - answered by the
owner in sec 19.

## 19. Design-fidelity round 6: chip removal + workspace identity (2026-10-26)

The owner answered both open items in one message ("okay remove it,
another thing to put in mind the color theme is not match i want to
clarify about something when i change workspaces this `1:powershell1`
in blue color doesn't change - you remove it or match it with the
current running workspace"):

- **`cmux-win11` chip removed** ("okay remove it"): reverses the sec-16
  keep-back (the chip stayed because `version display removed` did not
  name it; it is the design's product tag, nodes 1:397/1:398). The
  paint block and the right cluster's chip-anchored edge left
  `draw_status_bar`; the UTF-8 and `pipe:` leaf segments now anchor to
  the strip's right pad (`bar.right() - 8`). Deliberate owner
  divergence from the mock - class-recorded like the band and the
  version strip.
- **The blue marker now visibly tracks the current workspace**
  ("remove it or match it" -> "match it" chosen and disclosed): the
  strip leads with the active workspace's identity segment
  `w1 · powershell` (secondary ink, minted `w{N}` id + the live name),
  then the group separator and the marker run. The marker run already
  read the active workspace by construction (markers build from
  `workspaces.active`'s tree); same-shell pane titles made the strip
  read IDENTICALLY across workspaces - and the live derived name alone
  would reproduce exactly that ("powershell" in every workspace), so
  the minted `w{N}` id is the stable identity that visibly changes on
  a switch. The a11y label rides the painted label
  ("Status bar, {ws} workspace, {n} pane(s)").

**The fold matrix (round-6 arithmetic):** the identity segment shares
the marker budget, so `draw_status_bar` resolves two flexibles by
priority before laying markers out - the leaf tail (14 -> 8 codepoints
via `STATUS_IPC_LEAF_SHORT`) flexes first, then the ws NAME (the id
always stays; the helper is `status_ws_fit_case`). The marker run and
the minted id never fold; `free_leafless` subtracts BOTH right-cluster
anchor gaps (the 12px bar-pad and 12px UTF-8 gaps) so the helper's
inequality stays the same one the paint pass applies.

Two lessons recorded: (1) the first live binary carried a real bug the
fixture probe could not see on its geometry but the owner's session
surfaced on live capture - `markers_max_x` still subtracted `ws_span`
while the fit loop's cursor already started past the painted ws
segment, so the segment was counted twice and one-pane marker runs
fell behind a bare ellipsis while the full label painted (magnified
strip crops locate the ellipsis at the marker start; a temporary
`strip-fit` eprintln diagnosed the frame values and left the binary).
The fix keeps `markers_max_x = right_clamp - 10 - telemetry_span` and
a comment on why the ws span must not ride it. (2) chip-absence
predicates need fill-RUN rules once text lives where the chip was: the
re-anchored leaf's glyph AA hits the exact fill color in isolated px
(verified identical-across-captures at (524,685)/(663,680)), so both
the behavior probe and the round-6 scanner assert no horizontal run
>= 6px and no fill-heavy row, not zero exact-px.

Verified live (owner session, `cmux_live_8.png` vs `cmux_live_7.png`,
client 1125x700 dpi 120): the strip reads
`w1 · powershell | 1:powershell* | CPU: 26% MEM: 10 GB CLAUDE: IDLE ...
pipe: ...09267108-1001 UTF-8`; the focused marker's info-blue ink
restores at x 465..554 (186 px, byte-count equal to live7's 332..421
run shifted right by exactly the ws segment's 133px span). The
side-by-side scanner (`sidebyside_r6.py`, 24 checks; r5's
`sidebyside_final_r5.py` stays the round-5 record) passes 24/0 - S16R
(chip fill block + bbox runs absent), S-WS (identity ink 264 px in
x 324..470, id glyph at the left pad), S19R (marker blue in its moved
zone), D3R (chip absence classified EXPECTED) - and the drift
attribution says the ENTIRE live7->live8 change is the status bar
(6524 px; aside, title strip, and terminal zones zero). Behavior probe
(`round6_ws_probe.py`, fixture instance, 14/14): launch, `workspace
create` mints `w2` and activates it, `workspace select`/`current`
server-state assert; captures A/B bracket the switch and the identity
zone (x 322..380) visibly changes (61 px).

Unit pins added: `status_workspace_label` (id + name composition,
`STATUS_WS_MAX_CHARS` cap), `status_ipc_label_capped` (both leaf
variants), `status_ws_fit_case` (the four-case priority matrix),
`fold_label` (the shared head+ellipsis fold).

## 20. Design-fidelity round 7: whole-application palette audit + the browser default background (2026-10-26)

The owner's clarified complaint: "no i mean the color theme for the
whole application dosen't match the color theme of the figma" - so
round 7 stopped probing single widgets and audited every painted pixel
of the window against sec 2. First the owner's own config was ruled
out: `C:\Users\noah\.config\cmux\cmux.json` carries no `theme` block
(appearance/panes/terminal/shortcuts only), so the divergence claim
could not rest on user overrides and a paint-level audit followed.
(The Figma MCP was re-attempted this round per the once-per-round
rule: `create_session` succeeded - session `1980fa13-7a62-4532-
9fcb-ced048170337`, file `AGKdhDGDEmQieIRKzc7ain`, root node `1:2`
"cmux - Minimal Terminal Variant" - but BOTH `get_screenshot` and
`get_metadata` stopped at the Starter-plan rate-limit paywall again,
so the audit compares against secs 1-19's verified node reads, not
the live file; the file remains owner-side unreadable.)

**Full-window census (`cmux_live_8.png`, client 1125x700 dpi 120):**
the four sec-2 base tokens cover ~97% of the window - `#0c0e14`
66.02% (viewport/status + terminal backdrop), `#191b22` 27.24%
(sidebar; the sub-header segment was owner-removed in round 4),
`#1d1f26` 1.91% (chip fills), `#11131a` 1.64% (main bg + gutters).
Every off-token pixel belongs to hairline/edge/glyph AA: the window
top border `#202020` (1125 px at y=0), title-bar seal strokes
`#272c33` (y=39), the sidebar edge `#282d34` (x=318 - the sec-2
hairline blend rgb(40,45,52)). `cmux_live_9.png` (post round-7 code,
default landing) reproduces all four shares exactly.

**Palette overlay census (`cmux_r7_P.png`, family share 0.996):**
opened through a CLICK on the command bar - the screen offset is
window-rect left+680, top+20 (the in-app path, node 1:7818).
SendInput Ctrl+K proved unreliable at this DPI (the T0-vs-P control
capture differed only by 96 px, the status bar's CPU digits), so the
click is the standing opener. Overlay fill `#11131a` 23610 px,
selected row `#1d1f26` 13730 px, accent `#38bdf8` (highlight run)
over hover-digit `#36b5ee`-family AA. On-token.

**Terminal zone census:** 99.27% `#0c0e14` (the app's transparent
cell background over the design token fill); prompt text is pure
`#ffffff` (x 334..482 y 49..64). That is egui_tty's renderer default
(`ColorScheme::Dark => emulator.clone()` renders Ghostty's emulator
colors); sec 3 records the pane-body text as the designer's
illustrative mockup, NOT a cell spec, so re-theming terminal cells to
the design inks is a product-visible change - owner-gated candidate,
not implemented.

**The one off-token app-adjacent surface - the fresh browser leaf's
flat `#121212`.** Root-caused before any change: (1) the app paints
nothing there (browser panes render no fill; the child HWND covers
the rect); (2) Chromium's paint DOES reach PrintWindow captures - a
navigated page with an explicit color shows through (`#40ff80`
240,377 px); therefore (3) `#121212` is Chromium's own base paint.
WebView2's documented API default is white and the observed gray is
theme-derived: upstream issue 4389 (dark-mode put ignored; closed
pointing at the ControllerOptions early-init API) and 5752 (Runtime
154+: the blank-canvas base paint follows the WebView theme). This
machine runs Evergreen 156.0.4314.42.

**The fix (cmux-webview2, shipped this round):** `BrowserSpec`
gained `default_background: u32` (0xRRGGBB); the shell hoists its
resolved viewport fill (`self.runtime.theme.viewport_background` -
the design's `#0c0e14` token; a configured `theme.background`
reaches it through the same field) into the single `BrowserSpec::new`
call site. The controller is requested through
`ICoreWebView2Environment10::CreateCoreWebView2ControllerOptions` +
`ICoreWebView2ControllerOptions3::SetDefaultBackgroundColor` +
`CreateCoreWebView2ControllerWithOptions` (the documented
early-initialization path), the plain pre-Environment10 call remains
as fallback, and the wire-time `ICoreWebView2Controller2::
SetDefaultBackgroundColor` put stays as backstop; both failure paths
carry `log_once` diagnosability. `corewebview2_color` packs the token
opaque (alpha 255; the API accepts only alpha 0/255 and the
semi-transparent form needs a newer feature floor the port does not
require). Which of the two puts the runtime actually honors is not
attributable from capture evidence alone - both were active in the
probe; the records below describe the observed paint, not the
mechanism.

**Canvas ladder (round7_canvas_probe.py, one fixture):**
`about:blank` = flat `#121212` 240,478 px, byte-identical pre/post
fix; a PLAIN no-CSS page = `#0c0e14` 297,721 px with
`getComputedStyle` html/body both `rgba(0, 0, 0, 0)` - the
default-background mechanism IS live and paints the token under any
page with no defined background; an explicit token page = 298,313 px
of `#0c0e14`; the green gate sanity-passed. Conclusion: the token
base is real, and the gray survives ONLY on the bare `about:blank`
leaf - Edge special-cases the blank document, which is also M5.1's
default landing URL. Residual owner-gated options: (a) keep
upstream's `about:blank` default (upstream parity), or (b) make the
landing page a token-painted data URL (product-visible: alters
`browser.url.get`/the m51 default page). Not implemented
unilaterally.

**Gates and battery:** fmt 0, clippy `-D warnings` 0 (exit codes
verified), UNPIPED workspace tests 373/0 across 24 suites (+1: the
`corewebview2_color` pack pin with a warm-byte guard), `cargo build`
0; the m51 browser battery re-ran against the rebuilt binary -
`smoke complete: 99 passed, 0 failed`, exit 0 (the spec grew a field;
the battery is its regression line). Probes kept: round7_theme_probe
(census fixture incl. the palette click + the green disambiguator,
with fixture/stderr retention) and round7_canvas_probe (the ladder);
captures `cmux_r7_T0/P/B1/B2/C.png`, `cmux_r7c_blank/plain/token.png`,
`cmux_live_9.png`.

**Remaining "doesn't match the figma" candidates left for the owner
to pinpoint:** (1) terminal cell colors - Ghostty emulator defaults,
not spec'd by the design (above; owner-gated); (2) the chrome
typeface - `fonts.rs` orders the chrome family Ubuntu-Light first
with Segoe UI Bold/Consolas companions, while sec 3's mockup faces
are Liberation Serif (display/serif segments) and Inter; (3) the live
Figma file itself may differ from the recorded export (paywalled,
unreadable from here). The pixel evidence says the application
palette IS the sec-2 palette; every candidate is an owner decision,
not an app-paint defect.

## 21. Owner round 8: pane markers retired + the sec-20 residuals executed (2026-10-27)

The owner answered with two directives: "i want you in the status
bar to remove 1:powershell* and keep going" and "for my decision
just do the recommend".

- **The pane-marker run retired.** The tmux-style per-pane markers
  (`ordinal:title`, `*` on the focused one - the design's `1:zsh*`
  mock) no longer paint; the round-6 workspace-identity segment now
  LEADS the strip alone, followed by the group separator, the
  machine/agent telemetry (round 3's 10px), and the right cluster.
  Deliberate mock divergence, recorded like the chip. What left with
  the run: the `status_marker_label` helper + its
  [`STATUS_MARKER_MAX_TITLE_CHARS`] cap + two unit pins; the strip's
  fold matrix lost its `marker_needs` term (`status_ws_fit_case`
  free-checks [ws + leaf] as a pair now); the marker
  fit/measure/paint/overflow-ellipsis passes went with it. The a11y
  strip label still counts the workspace's real panes ("Status bar,
  {ws} workspace, {n} pane(s)") - the count is real information even
  though the strip no longer enumerates them. The round-6 ws probe
  flipped its focused-marker assertion to marker-RETIREMENT rules:
  no info-blue ink anywhere left of the right cluster in an agent
  -less fixture (a per-pixel shape test - blue >= green+20, blue >=
  red+60, blue >= 165 - because the round-6 tol-90 loose box also
  matched the identity label's secondary ink: 233 px of false
  positives), and no secondary-ink label in the former marker zone;
  15/15 on the round-8 binary.
- **The sec-20 residual executed the recommended way: the browser
  landing.** NEW browser leaves (`browser.open_split` without a URL)
  now land on a token-painted data-URL home -
  `browser_home_url(viewport_background)` builds a title-less,
  content-less page whose body background IS the resolved viewport
  fill (percent-encoded; a configured `theme.background` moves the
  home with the chrome). The fresh leaf therefore shows the design
  token the moment it exists instead of Edge's theme-gray blank
  document (which the runtime special-cases against every
  DefaultBackgroundColor mechanism per sec 20's ladder). Restored
  leaves keep their SAVED URL: older `about:blank` leaves stay
  exactly as saved (the m51 restart semantics unchanged - the
  restored leaf reloads the saved page). Recorded divergence from
  upstream's configurable-preferences default (`about:blank`, M5.1).
- **The keeps (recommended, no change):** terminal cells stay the
  Ghostty emulator defaults (the design does not spec cells - sec
  20's census), and the chrome typeface stays the current family
  (Ubuntu-Light/Segoe UI/Consolas; Windows-native, and rounds 1-6's
  pixel reconciliations were tuned against it - switching to the
  mock's Liberation Serif/Inter faces would re-break every fold
  width for one face-swap).
- **Verification:** fmt 0, clippy `-D warnings` 0, UNPIPED workspace
  tests 372/0 across 24 suites (two marker pins retired, one
  `browser_home_url` pin added - the token pack and the byte-order
  generalization), `cargo build` 0; the m51 browser battery re-ran
  on the rebuilt binary - `smoke complete: 99 passed, 0 failed`,
  exit 0 (its open/default/restart legs now exercise the token
  home); the round-6 ws probe 15/15 (updated predicates above).
- **Live census (`cmux_live_11.png`, owner relaunch):** on-token
  97.02% (`#0c0e14` 66.08%, `#191b22` 27.39%, `#1d1f26` 1.91%,
  `#11131a` 1.64% - visually unchanged from live_8/live_9 covers);
  the strip shows identity ink 238 px at x 324..430, zero
  info-blue px, and the CPU/MEM muted digits now sit where the
  marker used to; drift vs `cmux_live_10.png` is 2305 px and ALL of
  it is inside the strip band (the removed marker zone + the
  telemetry's left shift); the rest of the window is byte-identical.

The residual decisions and keeps in this
section execute under
ADR-0016 (owner decision precedence): the owner's standing rule
that delegated product/design decisions run the recommended
option, disclosed with rationale rather than parked.
Architecture/scope/licensing/security decisions and items where
the real Figma file is the deciding evidence (and unreadable under
the paywall record) remain exception-gated.

## 22. Round 9 landed: sidebar depth — wheel scroll, drag-reorder, inline rename (2026-10-27)

The owner asked for sidebar depth matching upstream: overflow
scrolling, drag-reorder, and rename inside the strip. The design file's
aside (node `1:399`) carries no scroll/drag mock affordances beyond the
row list itself, so the interaction semantics port upstream behavior
(`SidebarDropPlanner`, `SidebarInlineRenameCommit`, the ScrollView
reveal rules) onto the verified geometry.

**Wheel scroll.** The workspace rows AND the sec-12 services segment
are ONE scroll column (upstream's `LazyVStack` rule: the aside scrolls
as a unit; sec 12's fixed rows `132..158/160..186` are the scroll-0
positions). The wheel actuates only over the sidebar's content region
(the terminal lane's hit region excludes the aside, so no event
stealing); one notch exercises ~12.75pt of smoothed scroll (egui's
smoothed delta drains over ~10-40 frames). Edge fades paint as
solid-fill alpha ramps over a Mesh (16pt, vertex pairs at fractions
0.0/0.05/0.25/0.65/1.0, alpha `(1-fraction)·255`) — divergence from the
mock's implied scrim gradient, recorded. A selection change reveals its
row with minimal motion (rows above align their top to the strip top,
rows below their bottom; visible rows keep the scroll), which is what
makes IPC `select(0)` a deterministic scroll-0 pin and `select(11)` a
bottom-edge pin (140pt at the probe's 242pt viewport).

**Drag-reorder.** Upstream's `SidebarDropPlanner` arithmetic ported:
the pointer's content y maps to the NEAREST row boundary slot
(exactly-half rounds to the NEXT slot), the commit removes the dragged
row and inserts it at the slot with removal-adjusted target
(`suppressesNoOp`: a no-op drop commits nothing), the active selection
follows the moved row BY ID, a drag session's release frame never
selects, and the insertion indicator is a 2pt accent line inset 8 —
upstream's `SidebarReorderIndicatorView` shape. Divergence recorded:
no floating drag image; the source row dims and the accent line leads.
Drag autoscroll (24pt margin, linear ramp to full speed at the edge)
follows upstream's `SidebarDragAutoScrollController` shape and is what
lets a drop above the clip resolve its slot (the pointer crossing the
top margin scrolls the strip).

**Inline rename.** Double-click opens the row editor buffered with the
row's FULL display name (`customTitle` > process-title precedence from
`preferred_name`); commit is Enter or focus loss through upstream's
`titleToCommit` policy (trim; an empty result never writes; retyping
the auto-derived name freezes nothing; reconfirming a custom title is
a real edit); Escape cancels — in ONE press here, vs upstream's
two-press caret-park (recorded divergence; an AppKit caret affordance
the port's editor does without). The editor auto-cancels when its row
slides out of the scrolled window.

**Probe of record:** `test_sidebar_depth_r9.py` 35/35 on the final
binary — wheel round trip + terminal no-steal at pinned scroll 0,
exact planner click-drag, scrolled drag (active follows by id), rename
commit + Escape cancel, and a two-leg `customTitle` persistence round
trip. The battery pins strip states via the IPC selects above (the
reveal math) rather than pixel sensing: the chip `#1d1f26` and the
sidebar bg `#191b22` differ by 4/255 per channel, and only the ACTIVE
row paints — a pixel scroll sensor reads noise by construction.

**Deferred (owner-visible, not started):** per-workspace row colors,
WAIT/RUN/IDLE/ERR state rollup in rows, and the sec-12 metadata-row
increment. These keep ADR-0016's execute-the-recommended-option trail;
no architecture decision opened.
