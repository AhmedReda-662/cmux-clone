# ADR-0009: Session persistence (restorable pane tree)

*Status: Accepted (owner, 2026-10-18 — M2.4a owner-verified the same day)*
*Date: 2026-10-18*

## Context

M2.4 needs the window's pane tree to survive a restart. Upstream cmux
persists a full session and the store's semantics were verified before
designing the port's equivalence:

- **Snapshot root** — `AppSessionSnapshot: Codable, Sendable` carries
  `version: Int`, `createdAt: TimeInterval`, `windows:
  [SessionWindowSnapshot]`
  (`cmux-main/Sources/SessionPersistence.swift:1965`). A window holds
  `tabManager: SessionTabManagerSnapshot` (`selectedWorkspaceIndex`,
  `workspaces`, optional groups; line 1948). A workspace
  (`SessionWorkspaceSnapshot`, line 1825) holds ids/names/color,
  `focusedPanelId`, `layout`, and the authoritative `panels`
  (`SessionPanelSnapshot`, line 1747: id, `type` = `PanelType` raw string —
  `"terminal"`, `"browser"`, …, titles, directory, pinned/unread, git
  branch, listening ports).
- **Layout wire enum** — `SessionWorkspaceLayoutSnapshot`
  (`Packages/macOS/CmuxPanes/Sources/CmuxPanes/Values/`) hand-encodes as
  `{"type":"pane","pane":{panelIds, selectedPanelId,
  isFullWidthTabMode?}}` and `{"type":"split","split":{orientation,
  dividerPosition, first, second}}`; orientation raw values are
  `"horizontal"` / `"vertical"` (`SessionSplitOrientation.swift` — children
  left-to-right / top-to-bottom). Leaf `panelIds` are tab-strip order —
  the tab model: each split leaf stacks ordered panels with a selected
  one.
- **Store** — `SessionSnapshotRepository` (`…/Session/`): sorted-key JSON,
  atomic writes with identical-content skip, primary + `-previous` backup
  under `Application Support/cmux/`, version mismatch → unusable,
  `hasWindows` guard (empty-window snapshots are anomalous; empty states
  delete the file instead), unusable-primary recovery from the backup,
  richness `{workspaces, panels}` comparisons (panels first, workspaces
  break ties) gate overwrites and history retention. Richness grading plus
  per-8s autosave and history directories supply the crash-restore story;
  `SessionSnapshotHistoryEntry` keeps rotated history (out of scope here).

## Decision

New crate `port/crates/cmux-workspaces` (serde + serde_json; same model as
`cmux-config`) reshaping the verified subset:

- **Same wire shape, port subset**: `AppSessionSnapshot{version, createdAt,
  windows[].tabManager{selectedWorkspaceIndex, workspaces[]}}` with the
  same `workspaceId/processTitle/customTitle/customColor/currentDirectory/
  focusedPanelId/layout/panels` core; `PanelType` raw `"terminal"`.
  `SCHEMA_VERSION = 1` and every decoded snapshot must carry it plus ≥1
  window, mirroring upstream's unusability rules. Unknown fields are
  ignored on decode (upstream tolerance), and upstream-only fields the
  port does not model (agent/browser/markdown payloads, notifications,
  frame/sidebar, `workspaceGroups`, `scrollbackCapturedAt`) are simply
  absent from port-written files and tolerated when decoding upstream
  files.
- **Division of stores**: `<home>/.config/cmux/cmux.json` stays the
  user-edited config (ADR-0008); session state goes to the platform
  Application-Support equivalent `%APPDATA%\cmux\session.json` (+ a
  `-previous` backup; `CMUX_SESSION_FILE` overrides). Upstream keeps
  `~/.config` for config, Application Support for state — the port splits
  the same way rather than collocating both stores (machine state has no
  user-facing parity benefit that outweighs platform convention; portable
  use stays possible via `CMUX_SESSION_FILE`).
- **Save path (exit-time)**: eframe `App::on_exit` serializes the live tree
  (`TreeOutline` → layout enum; each leaf's pane carries its full tab list
  in `panel_ids`, selected pane = the visible one) into one workspace
  `"w1"`, saving depth-first after the terminal route: pretty JSON,
  identical-content skip, then rotate primary → `-previous`, then
  temp-file publish. `createdAt` is epoch seconds.
- **Restore path (startup)**: load (primary, then `-previous` on
  unusable), version/`hasWindows` gates, workspace picked by
  `selectedWorkspaceIndex` (clamped), then a repair pass replaces
  upstream's tolerance behaviors explicitly: duplicate panel entries and
  duplicate tab lists dedup, empty leaves drop (their split collapses),
  selections fall back to the first tab, missing panel entries synthesize
  as `"terminal"`, orphan panel entries re-attach to the first leaf, a
  vanished tree rebuilds from the panel list, unknown `focusedPanelId`
  falls back to the first pane, and out-of-range/non-finite divider
  positions clamp to 0.05–0.95 / 0.5. Every repair is logged; a snapshot
  that cannot be restored logs why and starts fresh (a broken session must
  not block the window, same policy as the config store). Restored leaves
  spawn real shells; restored ids are reused and the id counter seeds past
  every restored tab id so future `next_leaf_id()` can never collide.
- **Tabs in M2.4a**: the leaf structure matches upstream (multiple
  `panelIds` round-trip losslessly), but the UI renders only the selected
  tab until tab strips land (M2.4b) — extra tabs are kept on the pane and
  restored invisibly, with a log.

## Consequences

- Restarting the port reproduces the previous split layout, geometry,
  ordered panes, and focused pane, including after a crash (last exit
  state wins; nothing is written mid-session yet).
- One-owned backup only: upstream's rotated history directory and 8 s
  autosave are deferred (registered divergence; the incremental-save hook
  is where they slot in).
- Upstream `.json` files decode only as far as the port subset models
  them; macOS side files (window frames, sidebar state) are ignored —
  cross-platform snapshot *exchange* is explicitly NOT claimed.
- Deferred to later increments: workspace list switching (the format
  stores the full list already), tab strips (M2.4b), autosave cadence,
  comment-preserving saves don't apply here (machine data, pretty JSON
  like upstream's sorted-key output; field order is declaration-stable).

## Verification (M2.4a)

- `cargo test` workspaces: exact wire-shape byte tests for leaf/split,
  snapshot decode round-trip, upstream-field tolerance, version/empty/
  unknown-type rejection, unusable-primary backup recovery,
  identical-skip + rotation, and the repair rules (11 tests).
- `cargo test` shell bin: outline→workspace walk with tab membership,
  unknown-pane fallback, id-counter seed (3 tests); pane-tree: outline
  fidelity, `joined` rebuild with divider geometry, seeding (2 tests).
- Machine smokes (temp `CMUX_SESSION_FILE`): fresh exit saves the exact
  wire JSON; auto-split exit saves a 2-panes/1-split snapshot; a restore
  run logs `restored 3 pane(s)` and rewrites nothing (hash-identical
  store); a corrupt snapshot logs the reason, starts fresh, and self-heals
  on exit; a restart-no-smoke run restores 2 panes, auto-splits twice, and
  saves 4 uniquely-id'd panes (p1, p2, p3, p5 — the established shared
  leaf/split counter leaves the s-number gap, names stay collision-free).
