# ADR-0016: Owner decision precedence — "do the recommend"

- **Status:** Accepted (owner directive 2026-10-27, re-affirmed twice in
  the same session: "for my decision just do the recommend" and "now you
  done right and my decisions is do the recommend")
- **Date:** 2026-10-27
- **Supersedes:** none
- **Related:** the workspace standing rules (autonomous operation;
  escalation of licensing/scope/security matters), ADR-0006 amendment history
  (browser defaults), figma-design-reference.md secs 20-21 (the residual
  ledger this resolves)

## Context

Rounds 5-8 repeatedly converged on owner-gated forks: chip keep/remove,
marker match/remove, browser `about:blank` keep vs a token-painted home,
terminal-cell re-theming vs keep, chrome typeface swap vs keep. Each was
presented as a reasoned recommendation with pixel/behavior evidence, then
parked pending owner confirmation — slowing convergence and re-opening the
same question at the next round.

The owner resolved the pattern on 2026-10-27: after the round-8 execution
(marker retirement + the recommended residuals), they confirmed it "done
right" and set the standing rule for future decisions — **do the
recommended option**.

## Decision

For owner-gated product and design decisions, the recommended option is
**executed and recorded** when the owner delegates ("do the recommend"),
not parked:

1. The recommendation is stated in the round docs with its evidence basis
   and what the alternatives would have changed (the secs 20-21 pattern).
2. The executed choice lands as its own small, separable change (marker
   retirement, home URL, keeps - each independent), so a later owner
   override is cheap.
3. The round report always discloses what was executed - never silently.

Two standing exceptions, unchanged from the workspace rules:

- Decisions that materially affect **architecture, scope, licensing,
  security, or maintainability** still stop for explicit approval.
- Recommendations are not fabricated where **the real Figma file** is the
  deciding evidence and the file is unreadable (the MCP paywall record);
  such items stay surfaced with the concrete evidence request
  (e.g. one screenshot).

## Consequences

- Faster convergence; the fork ledger does not accumulate parked items.
- Documentation duty grows: every delegated decision must carry its
  recorded rationale, or the trail breaks.
- Recorded keeps (terminal cells, chrome typeface) and the recorded
  diversions (browser home) inherit this ADR as their authority.
