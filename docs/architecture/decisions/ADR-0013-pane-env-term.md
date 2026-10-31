# ADR-0013: Default pane environment — no forced `TERM`/`COLORTERM`

- **Status:** Accepted (2026-10-20 — behavior change landed in the
  owner-ratified M4.3 "terminal UX close-out" scope, live-verified; **owner
  ack 2026-10-21**)
- **Date:** 2026-10-20
- **Supersedes:** the pane-env part of the M2-era `shell_spec` (which seeded
  `TERM=xterm-256color` + `COLORTERM=truecolor` into every pane)
- **Related:** ADR-0003 (ConPTY), ADR-0008 (config `terminal.shell`),
  ADR-0012 (same milestone)

## Context

The port mirrored upstream's macOS app, which seeds terminal identity
variables into every spawned pane *only when unset*:

- `cmux-main/Sources/cmuxApp.swift:374-384` — `setenv("TERM", …)`,
  `setenv("COLORTERM", …)`, `setenv("TERM_PROGRAM", …)` if `getenv` misses.
  This is correct there because launchd GUI processes have no `TERM` at all;
  zsh/terminfo-driven tools would otherwise degrade.
- `cmux-main/CLI/cmux.swift:15131` — SSH sessions get
  `-o SetEnv COLORTERM=truecolor`.

The port's `shell_spec` seeded `TERM=xterm-256color` +
`COLORTERM=truecolor` into pane specs (dev box has no ghostty terminfo, so
upstream's `xterm-ghostty` value was not usable as-is).

## Evidence (M4.3, live)

With `TERM` set, PowerShell 7 under ConPTY behaves differently in exactly
the region M4.3 verifies:

- The `Ctrl+C` chord no longer cancels like Windows Terminal: the forced
  `TERM` accompanies PSReadLine's raw-VT input handling, so the interrupt
  echo + fresh prompt (`^C`) seen with `TERM` unset never happens; the
  recalled line survives the chord. Verified repeatedly through the battery
  with SendInput-injected chords (and pinned against the Arabic HKCU layout
  artifact that first masked the finding).
- With `TERM` unset — Windows Terminal's own behavior for ConPTY shells —
  the same injected `Ctrl+C` interrupts exactly like a real WT pane:
  `^C` echo + fresh prompt.

The mechanism (PSReadLine switching on `TERM` to bracketed/raw VT input
vs Windows-console processed input) is **strong inference**; the
with-TERM/w-o-TERM behavior difference itself is **verified fact** (two
battery operators, both directions).

## Decision

`shell_spec` no longer adds `TERM`/`COLORTERM`/`TERM_PROGRAM`. Panes
inherit the process environment as-is: Windows Terminal parity by default,
no touch of a user's pre-existing globals. The comment in `shell_spec`
carries the one-line rationale; this ADR carries the rest.

Revisit narrowly (not wholesale) if a concrete pane program misbehaves
without `TERM`: the remedy would be an opt-in config surface (a per-workspace
or per-shell `env` map — Post-MVP under ADR-0008), not another forced
global default.

## Consequences

- `Ctrl+C` interrupt semantics match Windows Terminal in pwsh (regression
  pinned by the battery's `^C`-interrupt check, and by
  `test_terminal_ux_m43.py` e2e).
- `TERM`-sensitive Unix-y tools (ncurses programs via WSL/Git Bash *inside
  the host pane* — not their own terminals) may assume no color capability
  until the opt-in env map lands. Windows-side programs (pwsh, cmd, cargo,
  git) do not depend on `TERM` for color; they use console VT detection.
- Divergence from upstream macOS is documented here and in the feature
  parity notes; on macOS the upstream seeding is the *no-op-safe* behavior,
  not something a shell would rather do without.

## Verification

- M4.3 battery 19/19 PASS (2026-10-20, twice on the final binary)
  including "ctrl+c interrupts the recalled line" — the check exists in the
  battery precisely because of this decision — plus paste/IME/wheel/resize
  unchanged by the removal.
- No test asserts `TERM` content (grepped); the M4.1/M4.2/CLI/M3.4 smokes
  all re-verified green after the removal (their panes run with the new
  default).
