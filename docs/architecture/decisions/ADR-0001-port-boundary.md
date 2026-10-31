# ADR-0001: The port lives outside `cmux-main/`; upstream stays reference-only

**Status:** Accepted
**Date:** 2026-10-17
**Deciders:** project owner (per workspace AGENTS.md §3); documented by the architecture lead

## Context

The workspace root contains only `cmux-main/` — an extracted snapshot of
upstream cmux (manaflow-ai/cmux), without `.git` and without initialized
submodules (`ghostty`, `homebrew-cmux`, `vendor/bonsplit`). The mission is a
Windows port with a Rust core, preserving cmux's product behavior.

The upstream tree is GPL-3.0-or-later (with BUSL-1.1 server directories) and
carries its own agent instructions (`cmux-main/AGENTS.md`). Modifying it in
place would (a) blur what is upstream behavior versus port behavior,
(b) entangle a new Rust codebase with Swift build assumptions, and
(c) conflict with the workspace rule that `cmux-main/` stays the reference.

## Decision

- The port takes the form of a **separate top-level Rust workspace directory**
  beside `cmux-main/` (directory name reflecting the product, e.g. `port/`).
  Nothing lives inside `cmux-main/` except reading.
- `cmux-main/` is treated as read-only product reference. Any change inside it
  requires explicit user request and follows `cmux-main/AGENTS.md`.
- The workspace root stays non-git until the owner decides otherwise; no nested
  git repository is initialized inside the tree (per AGENTS.md §3).
- Features are classified against `cmux-main/` behavior in
  `docs/architecture/feature-parity.md`; behavior preservation means matching
  observable product behavior of the reference, not its implementation.

## Consequences

- Reuse decisions (e.g. `cmux-tui` Rust crates) must respect licensing: the
  repo `LICENSE` is GPL-3.0-or-later for app/CLI/TUI crates, while the
  `cmux-tui` workspace Cargo metadata declares `license = "MIT"` — a
  discrepancy to escalate before copying code (see ADR pending on
  licensing review in `windows-port-plan.md`).
- Build tooling for the port is defined by the port workspace, not by
  `cmux-main`'s scripts; see `docs/development/building-windows.md`.

## Alternatives considered

1. **Modify cmux in place with a Windows backend** — rejected: trashes the
   reference value, violates workspace protection rules, and couples port code
   to upstream's Swift/Xcode build.
2. **Fork the whole upstream repo and develop Windows in-tree** — rejected:
   creates an implicit second product boundary without a documented seam;
   upstream snapshot here has no git remote to track anyway.
