---
name: spacing-audit
description: 'Audit padding, gaps and margins in UI code of any stack (Freya .padding/.spacing/.margin, egui Margin/item_spacing/add_space, gpui p_*/gap_*/px(), Tailwind and CSS spacing) against a 4/8pt scale, report every off-grid value as before/after, then snap them after the summary is shown. Use when spacing looks inconsistent, before a design handoff or release, when tightening a theme or token module, or when the user asks to check, align, clean up or standardize spacing. Adapted for Rust code from the Figma community spacing-audit skill. Usually invoked by the definitive-ux router; can be called directly.'
---

# Spacing audit

> **Stack:** Rust native crates load the `stack-map` skill for every API name. Web and mobile projects use `stack-map/maps/web.md` and the original web idioms. The rules below are stack-agnostic; only the fixes change idiom. Severity maps to the unified scale in the `definitive-ux` orchestrator when run through it.

Check a layout against a 4/8pt spacing scale, report every value that is off the grid, and snap it to the nearest correct step. The goal is consistent, predictable rhythm that is easy to maintain. Adapted from the Figma community `spacing-audit` skill for Rust source instead of auto-layout frames.

Load the `stack-map` skill first to resolve the framework and know which calls carry spacing.

## The scale

Base unit is 8. Use 4 only as a half-step for tight, component-internal spacing.

Allowed values: 0, 4, 8, 12, 16, 24, 32, 40, 48, 56, 64, 80, 96.

- 8, 16, 24, 32, 48, 64: structural spacing (section gaps, card padding, layout margins).
- 4, 12, 20: fine spacing inside a component (icon to label, chip padding, tight stacks).
- Anything not a multiple of 4 is off-grid and gets flagged.
- A multiple of 4 that is not a multiple of 8 at the layout level (section gap of 20 or 28) is "consider 8pt step", not a hard error.

If the project defines its own scale (a `spacing` module, `Theme` constants, egui `Spacing` struct set once, a gpui token file), audit against that scale instead and say so. Values that are multiples of the project's base are on-grid.

## What to check

| Framework | Padding | Gap between children | Margin and spacers |
| --- | --- | --- | --- |
| Freya | `.padding(n)`, `.padding((v, h))`, `.padding((t, r, b, l))`, `Gaps::new(..)` | `.spacing(n)` | `.margin(..)`, spacer `rect().height(Size::px(n))` |
| egui | `Frame::inner_margin(Margin::same(n))`, `Margin::symmetric(x, y)`, `button_padding`, `window_margin`, `menu_margin` | `item_spacing`, `icon_spacing`, `indent` | `Frame::outer_margin`, `ui.add_space(n)` |
| gpui | `.p_*()`, `.px_*()`, `.py_*()`, `.p(px(n))` | `.gap_*()`, `.gap(px(n))` | `.m_*()`, `.m(px(n))`, spacer `div().h(px(n))` |

gpui token suffixes map to px at a 16px root: `_0p5` 2, `_1` 4, `_1p5` 6, `_2` 8, `_2p5` 10, `_3` 12, `_4` 16, `_5` 20, `_6` 24, `_8` 32, `_10` 40, `_12` 48, `_16` 64. Tokens are on-grid by construction except `_0p5`, `_1p5`, `_2p5`, `_3p5`, which are half-steps and get the "consider" note at layout level. Literal `px(n)` forms are where off-grid values hide.

Also check gaps expressed as absolute positions: `.position(Position::new_absolute(..))`, egui `Rect` math, gpui `.top(px(n))`.

## How to audit

1. Resolve scope: the file, module, component or crate named in the request. With nothing named, the crate's `ui` or `components` module. State the scope.
2. Sweep with ripgrep for the calls above and collect every numeric literal plus the symbol it feeds. Freya and egui example:
   ```
   rg -n '\.(padding|spacing|margin)\(\(?[0-9.]+' src
   rg -n 'Margin::(same|symmetric)\([0-9]|item_spacing|add_space\([0-9.]+' src
   rg -n '\.(p|px|py|pt|pb|pl|pr|m|mx|my|gap)\(px\([0-9.]+' src
   ```
3. Walk the results top-down by file. For each value compare against the allowed scale.
4. For each off-grid value choose the nearest allowed step. When exactly between two, round to the one closer to the surrounding rhythm of that group.
5. Prefer consistency within a group over per-element correctness. If three cards use 14, 16, 16, align all to 16.
6. Use `codebase-memory-mcp` to see where a shared component or constant is used, so a fix on the token counts once with all its call sites listed.

## Rules

- Do not change layout structure, sizing behavior (`fill`, `flex`, `expanded`, `w_full`), content or styling. Only adjust spacing values.
- Preserve intentional exceptions and do not flag them: 1px borders and hairlines, optical alignment offsets (a 1 or 2px nudge on an icon), values driven by fill or flex, and spacing already bound to a theme constant or token.
- If spacing already comes from a token, leave it and note it instead of overwriting. If the token itself is off-grid, flag the token once.
- Round icon-to-label and other fine gaps to 4 or 8, not to larger steps.
- When a whole group is uniformly off-grid on purpose (a deliberate 6pt system, a 10px rhythm from a design file), flag it once and ask before mass-changing.
- Prefer extracting repeated literals into the project's spacing module when one exists; never create a new token system inside this audit.

## Output

1. Return the audit summary first, before changing anything:
   - Total call sites checked.
   - A before/after list of every off-grid value, grouped by file, in the form `src/ui/card.rs:42 Card padding: 14 -> 16`.
   - Values left untouched, with the reason (token, hairline, optical, fill-driven).
2. Apply the fixes only after showing the summary and only if the user asked to fix, or confirms. Then run `cargo check` and, if a snapshot test exists (`freya-testing`, `egui_kittest`), run it.
3. End with a one-line rhythm note: the dominant step the layout now uses ("layout on an 8pt rhythm, fine spacing on 4pt").

## Do not

- Do not resize elements or reflow content to hit a number.
- Do not invent a new scale. Stick to the 4/8pt steps above or the project's documented one.
- Do not touch corner radius, stroke, font size or line height unless the user explicitly asks.
