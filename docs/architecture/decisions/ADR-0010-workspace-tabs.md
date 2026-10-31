# ADR-0010: Workspace tabs (vertical sidebar) for cmux-shell

Status: Accepted (owner, 2026-10-18 — landed this session; acceptance given in-session)
Supersedes: none. Extends ADR-0007 (layout), ADR-0009 (sessions).

## Context

Upstream cmux's `newTab` (⌘N) does not create terminal tabs — it creates a
**workspace tab**, listed in a left sidebar. Verified in
`cmux-main/Packages/macOS/CmuxSettings/Sources/CmuxSettings/Values/ShortcutAction.swift`
and `ShortcutAction+DisplayName.swift`: `newTab` renders "New Workspace",
`nextSidebarTab` renders "Next Workspace", `prevSidebarTab` renders "Previous
Workspace", `closeTab` renders "Close Tab", `toggleSidebar` renders "Toggle
Left Sidebar" (⌘B). The sidebar persists via `SessionSidebarSnapshot` and the
tab order is `selectedWorkspaceIndex` in `TabManagerSnapshot` (see
`Sources/SessionPersistence.swift`, `AppSessionSnapshot`).

At the end of M2.4a the port had exactly one workspace: the pane tree existed,
session persistence covered one workspace, and hidden shells could not exist.
The owner picked option A of two M2.4b scopes (2026-10-18): workspace tabs +
vertical sidebar, over per-leaf panel strips (panel **tab bars inside a pane**
remain a possible later increment; the session DTO already stores their shape,
`panelIds`/`selectedPanelId`).

## Decision

1. **Model**: `App` holds a `Workspaces` list — `items: Vec<Workspace>` plus
   `active: usize`. Each `Workspace` owns its own `PaneTree<Pane>` and
   `focused` pane id, mirroring the upstream graph (window → tabManager ∈
   workspaces → per-tab layout + focused panel). Ids stay tree-local (`pN`,
   shared `sN` counter seeded per restored tree — ADR-0009 note unchanged).
2. **Sidebar (slice design)**: a fixed 216 px strip (`SIDEBAR_MAX_FRACTION`
   0.4, min 40 px) drawn after the panes so it never routes a click into a
   terminal: a ＋ button (new workspace) and one clickable row per workspace
   with an × close button. Active row uses the selection color; hover uses the
   hovered-widget color. No dragging, no context menu, no theming yet.
3. **Names**: upstream names a workspace after the running process
   (`processTitle`). The port derives the row name live from the focused
   pane's shell title (OSC, `egui_tty::Terminal::title()`), falling back to
   `shell {n}`. Rename UI/custom titles are deferred (the wire format already
   round-trips `customTitle`).
4. **Hidden workspaces stay alive**: inactive workspaces' panes are polled
   (`Terminal::poll()`) once per frame without rendering, so shells keep
   producing output while hidden. **Recorded limitation**: a hidden pane's PTY
   size is only refreshed by `Terminal::ui()` when its tab is shown; output
   written while hidden wraps at the last shown width until then.
5. **Actions**: `newTab`, `closeTab` (active workspace; the last workspace
   closes the window), `selectWorkspaceByNumber` (ctrl+1…9 rows, each bound
   from the digit key), `nextSidebarTab`/`prevSidebarTab` (cycle), and
   `toggleSidebar`. Sidebar clicks enqueue `Action::SelectWorkspace(index)` /
   `Action::CloseWorkspace { at }` and are applied after painting (a close
   invalidates row indices mid-iteration otherwise).
6. **Close semantics**: a pane exited inside a workspace closes that pane; a
   workspace's last pane closes the workspace; the last remaining workspace
   closing closes the window (same product decision as M2.2, now one level
   up). The selection-follow rule lives in
   `Workspaces::close` (unit-tested pure helper `adjusted_active_after_close`).
7. **Persistence**: `build_session_snapshot` walks **every** workspace and
   saves the active index; `try_restore` builds each workspace independently —
   a workspace that cannot be repaired/spawned is **skipped with a log**
   instead of discarding the whole session; the restored active index is the
   saved index clamped to what survived. The sidebar's own visibility flag is
   always restored as *shown* (upstream persists it; not yet stored —
   divergence noted below).
8. **Strokes (provisional, owner-accepted 2026-10-18)** — Windows-mapped from
   upstream's ⌘ preset by the `cmd` → `ctrl+shift` family rule, because plain
   ctrl+{n,q,b} would collide with shell conventions (documented in ADR-0008's
   mapping discussion):

   | Action id (upstream)        | Upstream | Port stroke       |
   |-----------------------------|----------|-------------------|
   | `newTab` (New Workspace)    | ⌘N       | `ctrl+shift+n`    |
   | `closeTab` (Close Tab)      | ⌘W       | `ctrl+shift+q`    |
   | `selectWorkspaceByNumber`   | ⌘1–9     | `ctrl+1`…`ctrl+9` |
   | `nextSidebarTab`            | ⌘⌃]      | `ctrl+shift+]`    |
   | `prevSidebarTab`            | ⌘⌃[      | `ctrl+shift+[`    |
   | `toggleSidebar`             | ⌘B       | `ctrl+shift+b`    |

   The digit family is bound row-wise from the stroke's **key** (`ctrl+5` →
   1-based row 5 → index 4; `ctrl+0` selects nothing). Per-stroke rebinds of
   `selectWorkspaceByNumber` are rejected for now (one stroke cannot express
   the whole family; the id is logged as "rebind not supported yet" and the
   default family survives). ~~Workspace reorder, rename, and color
   actions are deferred~~ **Round 9 (2026-10-27): reorder and rename
   landed** - `moveWorkspaceUp/Down` rebinds remain the deferred
   *keyboard* form (no upstream strokes exist for reorder/rename);
   their gesture paths are live instead: sidebar drag-reorder commits
   `Action::ReorderWorkspace{from,to}` through the planner verdict
   (`suppressesNoOp`; active follows by id) and double-click opens the
   inline rename editor whose commit rides `rename_commit` (upstream
   `SidebarInlineRenameCommit.titleToCommit` policy: trim, empty never
   writes, an auto-name look-alike freezes nothing). Per-workspace
   colors (`custom_color` round-trip support exists on
   `WorkspaceSnapshot`; the picker does not) stay deferred.

## Consequences

- Keyboard cycling (`Ctrl+Tab`) remains pane-level within the active
  workspace; `Ctrl+Shift+]`/`[` cycle **workspaces** — the two navigations no
  longer overlap.
- `Ctrl+Shift+Q` now closes the whole active workspace (its shells receive
  termination via ConPTY writer-drop), while `Ctrl+Shift+W` closes just the
  focused pane — deliberate, matches upstream's per-tab action set.
- The snapshot/time stability issue surfaced by fixtures (serde_json's
  default float writer decoding `created_at` one ulp off, breaking byte-equal
  save-skip and the backup-fallback test) is fixed by enabling serde_json's
  `float_roundtrip` feature (ryu writer) in `cmux-workspaces`.
- Session restore is now per-workspace tolerant; a corrupt workspace loses
  only itself.

## Evidence

- Upstream tab≡workspace action ids and display names:
  `ShortcutAction+DisplayName.swift` / `ShortcutAction+Defaults.swift`
  (`toggleSidebar` ⌘B, `closeTab` ⌘W).
- Snapshot graph with `selectedWorkspaceIndex` + `sidebar`:
  `cmux-main/Sources/SessionPersistence.swift` (`TabManagerSnapshot` L1948).
- `egui_tty::Terminal::poll()` used for hidden-alive invariant; `request_focus`
  reused for per-workspace focus settle (M2.4a restore path).
- Machine smokes (2026-10-18): fresh-exit 1-workspace wire JSON; hand-built
  3-workspace fixture restores as "3 workspace(s) with 4 pane(s) total" with
  the store byte-identical afterwards (SHA-256); auto-split → 3-pane save →
  restore round-trip; one-shot never restores/saves (`ONESHOT_FINAL_EXIT=0`).

## Status after implementation

Tests: 82 green (`cargo test`; cmux-workspaces 11, cmux-layout 27,
cmux-pty-port 2, pane-tree 15, cmux-shell bin 16, cmux-config 11). fmt +
clippy `-D warnings` clean.

**Round 9 update (2026-10-27):** the strip's interaction stack landed -
overflow wheel-scroll (one scroll column incl. the services segment;
edge fades; reveal-on-select via `scroll_to_reveal`; drag autoscroll),
sidebar drag-reorder (planner-verified, active-follows-by-id, 2pt accent
indicator), and double-click inline rename (upstream commit policy;
one-press Escape - divergence). Pure math lives in
`cmux-shell/src/sidebar.rs` (10 unit tests); the live battery is
`port/harness/test_sidebar_depth_r9.py` 35/35 on the final binary incl.
the `customTitle` persistence round trip. Suite at round close: 386/0
(24 suites). Still open increments: per-workspace colors, per-leaf
strips, hidden-pane PTY resizing; also deferred (documented in the
round-9 log): WAIT/RUN/IDLE/ERR state rollup rows and sidebar metadata
rows.
