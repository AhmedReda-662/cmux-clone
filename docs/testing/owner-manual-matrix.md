# Owner manual-verification matrix

What only a human can confirm: fidelity ("does it *feel* like a real
terminal?"), real IME composition, toast/hover visuals, and the OS states no
fixture can fake (an owner sitting in front of the machine). Everything here
maps to `feature-parity.md` rows; machine-verified claims live there, not
here. Work top to bottom; tick the checkbox when a section fully passes.

## How to run the port for a session

```powershell
cd D:\workspace\Projects\cmux-clone\port
. .\scripts\env.ps1          # PATH/toolchain for the session
cargo run -p cmux-shell      # runs target\debug\cmux-shell.exe
# run logging (optional): cargo run -p cmux-shell 2> app.log
```

- Config lives at `%USERPROFILE%\.config\cmux\cmux.json` (JSONC tolerated);
  delete it to get the built-in defaults (dark theme, pwsh panes).
- The startup log prints: config path, font %, gutter, resize step, theme,
  keybinding count, then `fonts: <the machine's faces>` — that `fonts:`
  line should list Consolas (this box has no Cascadia) plus Microsoft
  YaHei/JhengHei/MS Gothic/Malgun Gothic.
- The automation server pipe defaults to `\\.\pipe\cmux-<your-SID>`
  (visible in the log). For a second instance alongside, override with
  `CMUX_SOCKET_PATH` (see the CLI section).

## A. Launch + session basics

- [ ] App opens with one pwsh pane, modern prompt, no tofu/garbled text.
- [ ] The prompt is interactive: type, Backspace, arrow-up history works.
- [ ] `exit` (or ✕ on the window) closes the app; nothing lingers (check
      Task Manager: no `cmux-shell`, no orphan `conhost.exe`).
- [ ] Relaunch restores the workspace (panes, layout, focused pane).

## B. Workspace tabs + sidebar (rows 1, 2)

- [ ] `ctrl+shift+n` opens a second workspace; the sidebar gains a tab with
      a live shell-title name.
- [ ] `ctrl+1` / `ctrl+2` switch; `ctrl+shift+[/]` cycle; `ctrl+q` —
      actually `ctrl+shift+q` closes the active workspace.
- [ ] Clicking a sidebar row focuses that workspace; clicking inside a pane
      focuses it (title bar / autofocus feel).
- [ ] `ctrl+shift+b` hides and shows the sidebar; contents reflow.
- [ ] Run a long command in a *background* workspace tab; the tab shows an
      unread ring (warn-colored dot) — switch to it and the ring clears.

## C. Splits (row 3) — chords per `DEFAULT_BINDINGS`

- [ ] `ctrl+shift+r` splits right, `ctrl+shift+d` splits down; both panes
      stay usable (independent shells).
- [ ] `ctrl+tab` / `ctrl+shift+tab` cycle pane focus; the focused pane's
      border highlights.
- [ ] `ctrl+shift+w` closes the focused pane; one-pane trees close on last
      pane removal.
- [ ] `equalizeSplits` and `resizePane{Left,Right,Up,Down}` have **no
      default chords** on purpose — bind them in `cmux.json`
      (`shortcuts.bindings`, e.g. `"resizePaneRight": ["ctrl+shift+l"]`) and
      confirm the sidebar/equalize responds, or drag the divider with the
      mouse.

## D. Terminal feel (rows 5/6/7 — human-added on top of the machine lane)

Machine lane already proves typing/arrows/Ctrl+C/wheel/select/copy/paste
over injected input (M4.3 battery). The human adds fidelity:

- [ ] Typing latency and echo feel instant; selection dragging with the
      *real* mouse feels like Windows Terminal.
- [ ] `Ctrl+C` in pwsh cancels the current line with `^C` + a fresh prompt
      (parity with Windows Terminal — this is what ADR-0013 protects).
- [ ] Select text → `Ctrl+C` copies; `Ctrl+V` pastes; pasting a
      multiline block into a ready prompt works (the shell's paste wrapper).
- [ ] **Real IME** (Microsoft Pinyin): with a pwsh pane focused, switch the
      IME (win+space), type `nihao` → the composing window appears, commit
      `你好` → the grid shows wide CJK glyphs (font fallback ADR-0012).
      Japanese ROMAJI → kanji is the same check.
- [ ] Wheel-up scrolls into history; wheel-down (or any output) returns to
      the live tail without a stuck viewport.
- [ ] Drag the window edge through several sizes quickly — rendering keeps
      up, the grid rewraps, nothing glitches across DPI of the monitor.

## E. Notifications (row 8)

- [ ] From a pwsh pane, paste:
      `[Console]::Write("`e]9;hello from OSC 9`a")` — an unread ring appears
      immediately; if the workspace is *not* focused a desktop toast pops
      too.
- [ ] Same with the 777 form:
      `[Console]::Write("`e]777;notify;Title here;Body here`a")`.
- [ ] With `notifications` configured (`suppressWhenAppFocused`
      false-positive checks, `soundWhenFocused` on), confirm a focused pane
      stays quiet and an unfocused one sounds (subjective — your call).
- [ ] Real agent run: `claude` in a pane (typed shim), give it a trivial
      task — on completion you get the ring (inline, focused) or a toast
      (unfocused), no duplicate. This exercises the hooks shim + turn
      detector together.

## F. Persistence (row 22)

- [ ] With two workspaces + a split layout, close the app; relaunch shows
      the same tab order, split geometry, and focused pane. Config-heavy
      changes (theme) survive.

## G. Config + themes (ADR-0008, row 23 subset)

- [ ] Set `theme { scheme, background }` to a light scheme; relaunch = the
      palette + chrome colors follow.
- [ ] Rebind a shortcut (`shortcuts.bindings`) — the new chord works, the
      old default dies; a broken stroke falls back to the default + logs.
- [ ] Font zoom/gutter/resize-step values (the startup log echoes what it
      read) round-trip.

## H. CLI + automation (row 16, M4.1/M4.2 subset)

Against a *live* app (second terminal):

```powershell
cmux ping
cmux identify --json
cmux workspace new --json
cmux surface list
cmux send --surface 1 "echo hi from CLI"
cmux read-screen --surface 1
```

- [ ] Every verb answers; `read-screen` mirrors what you see in the pane.
- [ ] `--password` flows: set `CMUX_SOCKET_MODE=password` + a password, run
      the triangle (wrong/right/no-source) exactly as the smoke does
      (`port/harness/test_automation_m42_password.py` prints the shapes).
- [ ] Second instance: `CMUX_SOCKET_PATH=\\.\pipe\cmux-test2` + launch
      another `cmux-shell`, aim the CLI at one then the other with
      `--socket` — this is the M4.4 multi-instance lane's manual edge case.

## Known limits on this dev box

- Single 1.25×-scaled monitor: multi-monitor/DPI-crossing/window-movement
  between monitors cannot be verified here (row 5/7 criteria note it).
- Accessibility (UIA) is Post-MVP — no matrix row.
- Sound fidelity of `soundWhenFocused` is subjective; verify audibility at
  minimum.
