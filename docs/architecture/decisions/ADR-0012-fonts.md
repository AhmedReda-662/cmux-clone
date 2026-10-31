# ADR-0012: Windows system fonts for the grid (M4.3)

- **Status:** Accepted (2026-10-20 — implemented + live-verified under the
  owner-ratified M4.3 "terminal UX close-out" scope; **owner ack
  2026-10-21**)
- **Date:** 2026-10-20
- **Supersedes:** none
- **Related:** ADR-0002 (terminal engine), ADR-0004 (eframe/egui),
  ADR-0008 (config — custom fonts stay Post-MVP)

## Context

egui 0.36.2 renders owned fonts (`egui::FontDefinitions`), and its defaults
(epaint 0.36.2: Hack, Ubuntu-Light, NotoEmoji, emoji-icon-font — family
orders verified in `epaint-0.36.2/src/text/fonts.rs`) carry **no CJK
glyphs**. The engine renders IME input and shell output through those
families, so CJK text (filenames, localized prompts, OpenCode/Claude tool
output) shows as tofu on Windows.

Upstream resolves this without our own planner: the macOS app picks the
terminal font from the ghostty config (`GhosttyConfigDiscovery*`, parity
row 23) and non-covered codepoints fall through macOS's automatic font
cascade (NSFontManager), so every glyph finds a face. That cascade does not
exist for embedded egui text on Windows: egui's fallback resolution is
exactly the ordered family list we install.

The port's M4.3 lane needed CJK coverage with **no new dependencies** and
no drag on the render path (the engine builds one atlas; face selection is
startup-time).

## Decision

`port/crates/cmux-shell/src/fonts.rs` scans `%WINDIR%\Fonts` at context
startup and installs what the machine has:

- **Grid primary**, most preferred first: Cascadia Code → Cascadia Mono →
  Consolas. All keep a uniform advance under the terminal grid; the machine
  that lacks Cascadia keeps Consolas (this dev box: no Cascadia — the
  live-verified chain is Consolas-first).
- **CJK fallbacks, script-coverage order**: Microsoft YaHei (simplified
  Chinese) → Microsoft JhengHei (traditional) → MS Gothic (Japanese) →
  Malgun Gothic (Korean). `.ttc` collections load with index 0.
- **Family composition** mirrors epaint's defaults with our faces spliced:
  Monospace = primary + CJK + the four egui defaults; Proportional =
  Ubuntu-Light + CJK + the remaining defaults, so CJK resolves to a *text*
  face before the icon/emoji fonts get a chance (avoids an icon font's
  approximation of CJK codepoints).
- **Degrade silently**: a machine with none of the candidates (or any
  unreadable file) keeps egui's defaults byte-for-byte and logs why; one
  unreadable face drops only itself.

Face selection is logged once at startup (`fonts: …` — the M4.3 battery
carries a dedicated check for that line), and the pure planner/composer
sits behind unit tests (fake filesystem + fake font bytes; real
`%WINDIR%\Fonts` reads happen only in `install`).

## Consequences and divergences

- **Divergence from upstream (recorded):** upstream's font stack is
  settings-driven (ghostty config); the port picks Windows-identity faces
  until the custom-fonts milestone (ADR-0008's row-23 criteria). No per-font
  config knob yet — a machine-specific chain is established purely by file
  presence.
- CJK **display** coverage is the scope; real IME composition entry
  (Microsoft Pinyin/Japanese ROMAJI composing into the pane) stays on the
  manual matrix (parity row 5) — the M4.3 battery verifies the normalized
  burst path (`IM on 漢字テスト` grid reads); seeing **wide glyphs** from
  the fallback face is verified via the same engine-layout path.
- Face loading happens once before the first frame is laid out (the log
  line prints at eframe creation), and the per-frame cost is just egui
  shaping text against a family list that grew by ≤5 entries.

## Verification

- Unit: 7 tests in `fonts.rs` (primary preference, Consolas fallback,
  no-disk defaults, coverage order with gaps, family composition + registry
  keys, unreadable-face skip, all-unreadable no-op) — the workspace's
  238 → 245 delta.
- Live: M4.3 battery 19/19 PASS (2026-10-20), including "fonts log line
  carries the machine faces" and the unicode-burst grid check with
  wide-cell normalization on this machine's
  `Consolas, Microsoft YaHei, Microsoft JhengHei, MS Gothic, Malgun Gothic`
  chain.
