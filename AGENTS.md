# cmux Windows Port — agent instructions

Standing rules for every session in this workspace. `cmux-main/` is the upstream
cmux checkout and ships its own instructions —
[cmux-main/AGENTS.md](cmux-main/AGENTS.md) (verified commands, verification
ladder, tagged builds, license map, safety rules). Follow those when working
inside that tree; everything below governs the port.

## 1. Mission

Build a Windows-first port of cmux — a Ghostty-based macOS terminal whose tabs
carry AI coding agents — in Rust, preserving its workflows and product behavior.
A real port: not a mockup, not a superficial UI clone, not an automatic
Swift→Rust translation. `cmux-main/` is the primary product reference; REA
(MCP server `rea`) is the evidence tool for investigating it; installed agent
skills are specialized workflows to discover and use when relevant.

## 2. Operating principles

1. Understand the repository before modifying it; gather evidence before architectural claims.
2. Reuse existing cross-platform components only after verifying compatibility, API, licensing, and maintenance.
3. Small, independently testable changes over large rewrites.
4. Keep Windows-specific functionality behind clear interfaces.
5. Preserve existing behavior unless a Windows adaptation or explicit product decision requires otherwise.
6. Do not invent APIs, capabilities, test results, or builds.
7. Never claim a feature works without appropriate verification.
8. Never silently remove, stub out, or replace an existing feature.
9. Explain trade-offs and ask for approval when a decision materially affects architecture, scope, licensing, security, or maintainability.

## 3. Workspace protection

- Inspect `git status`, the current branch, and existing changes before editing; never discard, overwrite, reset, or revert user work.
- Do not replace, refactor, or "modernize" `cmux-main/` to make the port easier; it stays the upstream reference.
- The port lives in its own clearly separated directory/workspace crate, documented at the boundary — never inside `cmux-main/`.
- Do not initialize another Git repository inside the existing one; preserve upstream remotes and make the relationship to upstream explicit.
- No file deletion or destructive commands without explicit approval; no commit, push, publish, or release without authorization.

## 4. Required discovery workflow (before any implementation)

1. Read `cmux-main/`'s docs and config sources of truth: [cmux-main/AGENTS.md](cmux-main/AGENTS.md), [cmux-main/CONTRIBUTING.md](cmux-main/CONTRIBUTING.md), [cmux-main/docs/start-here.md](cmux-main/docs/start-here.md), manifests, submodule config, LICENSE, build scripts.
2. Map major modules, entrypoints, dependencies, platform-specific APIs (AppKit/SwiftUI, Unix sockets, shell/process assumptions), and external integrations (Ghostty, browser, relay/daemon, workers).
3. Trace representative user workflows end to end (UI entry → app state → OS integration).
4. Inventory the terminal engine: build requirements, public API, supported targets, license.
5. Discover what REA's installed tools actually support (check its docs/tool list — do not assume an analysis method) and what agent skills are installed, reading each skill's instructions before use.
6. Then produce an evidence-backed architecture proposal and implementation plan before large changes.

## 5. Evidence and REA policy

For every significant finding record: the question, the tool/method, the source
file/symbol/call path or runtime observation, the observed behavior, confidence
plus remaining uncertainty, and the effect on the Windows design. Classify
statements as verified fact / strong inference / hypothesis / unknown.

- Never claim runtime behavior from static evidence alone.
- Never reverse-engineer unrelated proprietary components, bypass access controls, or extract secrets; do not violate licenses or laws.
- Store findings in concise project documentation (see §14), not as dumps in source files.

## 6. AI skills policy

- Discover the skills actually available in the environment; read the full instructions of any relevant skill before applying it.
- Prefer one well-matched skill workflow over many overlapping ones; follow its prerequisites and tool requirements.
- Treat generated suggestions as hypotheses until verified; never fabricate skills, tools, capabilities, or results. If a required skill is unavailable, disclose the limitation and proceed with documented alternatives.

## 7. Architecture requirements

Keep the app modular and testable across these provisional boundaries: desktop
UI/window lifecycle; application state and workspace orchestration; terminal
process lifecycle and I/O; terminal emulation and rendering; workspace/tab/split
layout; agent detection and notifications; browser integration; CLI and local
automation API; git/workspace metadata; configuration and session persistence;
Windows OS services.

- Rust is the primary language for the new core; other languages need a verified dependency, native-integration, or build justification.
- Do not assume a web UI, Tauri, any particular Rust GUI framework, ConPTY, or a specific terminal engine up front — evaluate alternatives on evidence.
- Record final boundaries and framework choices in an architecture decision record (§14).
- Verify the current compatibility and public API of every selected native dependency; keep a fallback plan if the preferred terminal engine cannot be built or embedded reliably.

## 8. Windows compatibility

Treat Windows as the primary target: ConPTY and child-process management;
PowerShell/Command Prompt and shell integrations; UTF-8, IME/graphemes,
clipboard, escape sequences; keyboard/mouse/focus/selection/resize; window
lifecycle, DPI, multi-monitor, accessibility; paths, permissions, config
locations, session recovery; notifications, local IPC, startup. x64 first;
extra architectures only when justified.

Do not confuse pseudo-terminal process management with terminal emulation or GPU
rendering — three separate responsibilities.

## 9. Feature preservation

Build the feature inventory from `cmux-main/`'s actual source, not assumptions:
workspaces and switching; tabs, vertical tabs, split panes; terminal sessions,
input, scrollback, resizing; persistence/recovery; agent detection, notification
rings and history; git branch/PR metadata; listening-port and working-directory
metadata; embedded browser + automation API; CLI/IPC automation; SSH and remote
workflows; custom commands and shortcuts; configuration, themes, fonts,
accessibility.

Classify each: initial-MVP / post-MVP / optional / blocked by dependency or
platform limit / not applicable to Windows — with evidence and acceptance
criteria. Do not promise full parity until demonstrated. Note: the existing
`cmux-tui/` Rust crates are the most likely reuse candidates (cross-platform
Rust with their own test lanes) — verify licensing before importing anything.

## 10. Quality requirements

- Separate business logic from OS integration; keep Windows specifics behind interfaces; avoid needless global mutable state and UI-thread blocking; use async where it helps, without added complexity.
- Errors carry actionable messages and diagnostic context; do not log terminal input, environment secrets, credentials, or sensitive output.
- Test core logic with unit tests and OS boundaries with integration tests where practical: terminal lifecycle, process cleanup, concurrent sessions, resizing, persistence, error recovery. Regression-test every significant bug fix.
- Run the relevant formatter/linter/tests/build after changes; record what could not be executed and why. Never suppress compiler errors, failing tests, or security warnings to obtain a green build.

## 11. Security and privacy

- Validate and authorize every IPC/local-control request; protect local sockets and named pipes against unauthorized access; keep session data and credentials out of logs.
- Do not execute commands received from an agent, a browser, or an IPC client without validation and authorization.
- Secure defaults, no telemetry by default; document any exception.

## 12. Licensing and attribution

- `cmux-main/`'s license is split per directory: GPL-3.0-or-later for the app/CLI/most crates; BUSL-1.1 for the server directories (`web/`, selected `workers/`, `services/iroh-relay-minter/`, `cmux-tui/relays/cloudflare-do/`) — the exact list is in [cmux-main/AGENTS.md](cmux-main/AGENTS.md).
- Read LICENSE/NOTICE/attribution files for every source file, submodule, dependency, and service before copying code; preserve required notices; document restrictions on redistribution or commercial use.
- Escalate genuine licensing uncertainty; do not guess.

## 13. Implementation workflow (per milestone)

State objective and acceptance criteria → inspect relevant source/deps → propose
the smallest viable implementation → name risks and unresolved decisions →
implement one narrow coherent change → run relevant checks → review the diff for
regressions, unsafe behavior, scope creep, unnecessary deps → update doc + §14.
Do not start several unrelated features before the foundation is validated.

## 14. Documentation

Maintain as the project grows (adapt to the existing documentation structure
rather than duplicating it): `docs/architecture/repository-map.md`,
`docs/architecture/windows-port-plan.md`, `docs/architecture/decisions/`,
`docs/architecture/feature-parity.md`, `docs/research/rea-findings.md`,
`docs/development/building-windows.md`, `docs/testing/test-strategy.md`.

## 15. Definition of done

A milestone is complete only when its acceptance criteria are met; the
implementation actually exists (not simulated); relevant tests and checks ran
and results are reported; failures and limitations are documented; the diff was
reviewed; documentation and feature status are accurate; and no unrelated user
changes were overwritten.

## 16. First task

Do not implement the whole application. Start with repository discovery
(§4), REA-assisted investigation (§5), installed-skill discovery (§6), the
feature inventory (§9), and an architecture proposal. Then recommend a minimal
vertical slice proving a real Windows shell can launch, render, interact,
resize, and shut down using the proposed Rust architecture — and wait for
approval before committing to major framework choices or a large rewrite.
