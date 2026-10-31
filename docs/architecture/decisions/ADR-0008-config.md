# ADR-0008: Config file (`cmux.json`) — format, location, keybindings

*Status: Accepted (owner, 2026-10-18 — M2.3/M2.5 landed and owner-verified)*
*Date: 2026-10-17*

## Context

The M2.3 increment needed per-app settings (font size, pane gutter, pane
resize step, custom shell, keybindings) to leave hard-coded constants.
Upstream cmux already has a config **file** — not just macOS UserDefaults —
and its shape was verified before designing the port's equivalent:

- **Location** — `CmuxConfigLocation.swift` documents the primary file as
  `<home>/.config/cmux/cmux.json` with a legacy fallback
  `settings.json` (`Packages/macOS/CmuxSettings/Sources/CmuxSettings/Stores/`).
- **Format** — `JSONConfigStore` docs: "JSONC (`// line` and `/* block */`
  comments, trailing commas) is tolerated on read via the injected
  `JSONCSanitizer`"; writes are **text edits at the target JSON path**
  (`JSONCPathEditor`) so comments and ordering survive, published via
  temp+rename (`JSONConfigAtomicPublisher`).
- **Semantics** — typed `JSONKey<Value>` ids with explicit defaults; a
  missing file decodes to defaults; malformed values decode to the default
  rather than failing; unknown fields are ignored; a file watcher hot-reloads
  subscribers (coalesced, value-deduped).
- **Keybindings** — `shortcuts.bindings` holds a flat action-id → binding
  map; `ShortcutKeymapBinding.swift` fixes the hand-editable form: a stroke
  string (`"cmd+d"`), a two-stroke chord array (``["ctrl+b", "c"]``), or
  `null` to unbind. `ShortcutAction.swift` provides the stable ids, including
  `splitRight`, `splitDown`, `equalizeSplits`, `focusNextPane`,
  `focusPreviousPane`, `resizePane{Left,Right,Up,Down}`.
- **Fonts** — global font magnification is an `Int` percent, default 100
  (`GlobalFontMagnification.percentKey` via `@AppStorage`).

## Decision

New crate `port/crates/cmux-config` (serde + serde_json, MIT OR Apache-2.0)
with the mirror shaped above:

- **Same file path on Windows**: `<home>/.config/cmux/cmux.json`
  (`%USERPROFILE%\.config\cmux\cmux.json`) — deliberate parity with upstream
  rather than the Windows-native `%APPDATA%\cmux\` convention; `user_config_path()`
  computes it and the shell honors a `CMUX_CONFIG_FILE` env override (tests
  and experiments). **Consequence:** users get one consistent location across
  platforms; diverging from Windows conventions is a later trivial rename if
  needed.
- **A first-run template is written** when the file is missing (raw JSONC
  with explanatory comments; parse machines never depend on the template —
  loading a missing file decodes to defaults, same as upstream). The template
  pre-populates this milestone's relevant keys.
- **Initial field set** (exact upstream ids where they exist):
  `appearance.fontMagnificationPercent` (default 100, clamped 50–300 at
  application), `panes.gutter` (default 12.0), `panes.resizeStep` (default
  40 px), `terminal.shell` (`{ program, args }` or null → Windows default
  shell), `shortcuts.bindings` (actions with the five previously
  owner-accepted provisional strokes as defaults; `equalizeSplits` and the
  four `resizePane*` actions supported but unbound by default).
- **Binding matching** uses egui `Modifiers::matches_exact` (command/ctrl
  correct by construction). Stroke grammar: modifier tokens `ctrl`, `shift`,
  `alt` first, key last; `cmd`/`win`/`super`/`meta` tokens map to
  `MAC_CMD`, which can never fire on Windows — there the command key IS
  Ctrl, so defaults use ctrl-based strokes. User override semantics mirror
  upstream: a stroke replaces, `null` unbinds, a missing action keeps its
  default, and an unrecognized stroke **falls back to the default** (logged)
  rather than failing the load.
- **Font application**: the terminal grid font (egui_tty `TerminalStyle.font`,
  base 12 px monospace) is scaled by the percent each frame. Everything the
  app renders today is terminal text, so this *is* global magnification for
  now; UI-chrome text appears at the menus/settings milestone.
- **Gutter/resize application**: `panes.gutter` replaces the renderer's
  `PANE_GUTTER` constant (renderer-side chrome only); `panes.resizeStep`
  feeds `PaneTree::resize_pane` for the `resizePane*` actions.
- **Startup**: a decode or read error logs an actionable message and falls
  back to defaults — a broken config never blocks the window.

## Divergences from upstream (deliberate, logged)

1. **Saves are canonical pretty JSON** — comments in a hand-edited file are
   *not* preserved when the app saves. Upstream edits the original text at
   the JSON path (comment-preserving). Port's comment-preserving editing is
   deferred until the first app-side save actually exists (accepted risk: the
   milestone has no save-on-exit yet; users' files are only rewritten by
   explicit save calls).
2. **No file-watcher hot reload** — upstream re-reads on change. The port
   reads once at startup; reload comes with a menu/read-harness milestone.
3. **Chords parse but do not fire** — `["ctrl+b", "c"]` round-trips and is
   stored, with a logged warning that the default is kept; stroke sequencing
   needs an input state machine (post-M2).
4. **Default strokes are the owner-accepted provisional set** (Ctrl+Shift+R/D,
   Ctrl+Shift+W, Ctrl+Tab / Ctrl+Shift+Tab), not a port of upstream's macOS
   preset; keymap presets are a later feature. Reason: the owner reviewed and
   learned these defaults interactively on 2026-10-17; changing them silently
   would contradict step 9 of the operating principles.
5. **`panes.resizeStep` (40 px)** has no verified upstream value — the dock's
   performShortcutCommand implementation was not located during the evidence
   pass; the field is configurable so the constant can be corrected if the
   upstream step is found.
6. **`closePane`** is a port-added action id — upstream shortcut actions close
   whole tabs (`closeTab`); no single-pane close action was found to mirror.
7. **`theme` subset (added with M2 completion, 2026-10-18)** — Terminal cell
   colors take `theme.scheme` only (`"dark"`/`"light"`, mapping onto the
   engine's two built-in schemes; upstream follows macOS appearance). Chrome
   (pane-area fill, gutters, sidebar shade, optional cursor) is configured
   directly in `cmux.json` as `theme.background`/`theme.cursor` hex strings.
   Upstream instead drives terminal colors from Ghostty config files
   (`GhosttyConfig*`, "reads your ghostty config") — that parsing stays
   post-MVP (feature row 23). Rationale recorded as evidence: `egui_tty`
   0.2.0 exposes only the two built-in `ColorScheme`s (the emulator's default
   fg/bg/palette are set behind `pub(crate)` in its `colors.rs`), and no
   newer release exists, so arbitrary hex terminal colors would need a fork
   or the Ghostty-config subsystem — both deferred. Unknown `theme` values
   log and fall back per field (decode-to-default policy).

## Verification

- `cargo test` — 64 green total: 11 cmux-config (comments/trailing commas,
  string-preservation, escaped quotes, missing file → defaults, full decode,
  binding type/chord-length errors, save→load round trip incl. parent-dir
  creation, unknown-field tolerance, template parse, canonical binding
  serialization), 27 cmux-layout (unchanged), 11 shell binding-table tests
  (stroke parse/replace/unbind/fallback/chord-skip, defaults present, action
  ids resolve, font clamp), 13 pane-tree, 2 ConPTY smoke — plus `cargo
  clippy --all-targets -- -D warnings` and `cargo fmt --check` clean.
- Machine smoke (oneshot mode, exit 0 each): first run writes the template
  and logs defaults; `CMUX_CONFIG_FILE` pointing at a broken file logs
  "failed to decode config …" and falls back to defaults; a custom config
  logs font 175% / gutter 25.5px and reports **4 keybindings** (5 defaults −
  1 `closePane: null` unbind), proving decode → override → unbind end to end.

## Consequences

- Runtime behavior is now config-driven for fonts, gutter, resize step,
  shell, and all supported keyboard actions; smoke keys stay hardcoded
  (`CMUX_SHELL_AUTO_SPLIT_MS` remains UI-independent by design).
- The keys/live values are wired at three small seams (`shell_spec`,
  `TerminalStyle.font`, `gutter_inset`/`resize_pane`); later keys follow the
  same path.
- Upstream parity debts tracked: comment-preserving edits, hot reload,
  chords.

## Amendment 1 (2026-10-22): custom commands - the `commands` entries (M6 slice 1)

Upstream's `CmuxCommandDefinition` (cmux-main Sources/CmuxConfig.swift
1524-1599) and its executor (Sources/CmuxConfigExecutor.swift 159+)
define the custom-command feature verified this slice: a required
non-blank `name`, optional `description`/`keywords`/`restart`, and
exactly one of `workspace` (a full CmuxWorkspaceDefinition) or
`command` (non-blank); upstream types the sanitized text
(`sanitizeForDisplay` strips invisible-bidi scalars, then trims)
plus a newline into the current terminal, gated by a trust
authorization and an optional confirm dialog.

Decision (the port's decayed subset):

- `cmux-config` decodes a top-level `commands` array into
  `CommandDefinition` with upstream's validation text; a broken entry
  fails the whole config read (load falls back to defaults), like
  upstream's decode error. `workspace` is stored as opaque JSON so
  upstream-shaped files parse even though the port cannot launch
  them (recorded residual, answered with a structured problem).
- The port's run path (`cmux-shell`) re-reads the config at call
  time through a side-effect-free reader (first-run template write
  stays startup-only), then resolves the name. Unknown name is
  not_found; workspace flavor and unacknowledged confirm are
  invalid_state. `ack` on the wire (bool) replaces upstream's
  dialog - the CM-Divergence: scripts acknowledge with the CLI's
  `--yes`; no interactive dialog lane exists yet.
- Upstream's per-fingerprint trust ledger (per-project approval of
  config-sourced commands) is not ported: the port reads one
  user-owned global file today, the same trust level that rebinds
  shortcuts. Per-project config overlays (upstream's project
  command files) stay future work and would re-open the ledger
  question.
- The sanitize runs app-side (`cmux_config::sanitize_command_text`)
  so the type-into-terminal boundary keeps upstream's
  invisible-character defense without trusting the CLI copy.

Verification lives in test-strategy section 14 (gates, 307 unit
tests incl. the four new suites, smoke 49/49 x2, full battery).

**Status: landed for the M6 slice-1 subset** (upstream's palette
surfaces and workspace-flavor launches remain out); no records kept
for keywords (decoded, inert) and restart (workspace-lane only).
