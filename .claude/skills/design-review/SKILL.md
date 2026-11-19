---
name: design-review
description: 'Senior-style, prioritized design critique of a UI in any stack (Rust desktop with Freya, egui or gpui; web; mobile) from a screenshot, a running window or component source. Covers visual hierarchy, typography, spacing, color and contrast, motion, component states, window resizing, accessibility, copy and consistency. Returns findings ranked Blocking, Important, Polish, each with a specific fix in the project''s framework idiom, and can apply the safe ones with --apply. Computes contrast with a bundled script, judges renders with fourteen visual passes, and can score five dimensions 0 to 4 for audits and before/after baselines. Use when the user asks for a design review, crit, craft check or "does this look right" before a review, handoff or release. Adapted for local Rust code from Superfuture''s design-review skill. Usually invoked by the definitive-ux router; can be called directly.'
---

# design-review

> **Stack:** Rust native crates load the `stack-map` skill for every API name. Web and mobile projects use `stack-map/maps/web.md` and the original web idioms. The rules below are stack-agnostic; only the fixes change idiom. Severity maps to the unified scale in the `definitive-ux` orchestrator when run through it.

Adapted from Joey Primiani's Superfuture `design-review`. The rubric is his. The telemetry ping and the license-gated Pro tier were removed; everything runs locally against Rust source and a native window.

A senior-level craft critique. Opinionated and specific, not a checklist dump.

## 1. Acquire the artifact

Resolve the stack first: Rust crates load the `stack-map` skill; web and mobile use `stack-map/maps/web.md`.

- **Screenshot or image**: read it. Best for visual judgment (hierarchy, type, spacing, color). Ask for the window size and theme (light or dark) if not obvious.
- **Component or screen source** (`.rs`): read it plus the theme or token module it pulls from. Enables implementation-level fixes with `file:line` references and `--apply`.
- **Running app**: launch the app (the `run` skill in Claude Code, or the project's run command) and screenshot it at the default and minimum window sizes. For Freya, `freya-testing` `render_to_file` gives a deterministic render; for egui, `egui_kittest` snapshots.
- If you have neither a render nor code for a visual question, ask for one before guessing.

Index the crate with `codebase-memory-mcp` when the review spans more than one file so you can see how far a shared fix reaches.

## 2. Evaluate against the rubric


Load the platform file from `definitive-ux/references/platforms/` for the form factor; it sets the thresholds (hit areas, type sizes, focus model). Load `definitive-ux/references/slop-tells.md` for the purpose test and the tells.
Review every dimension in `checklist.md` (same directory):
hierarchy and layout · typography · spacing and rhythm · color and contrast · motion · component states · accessibility · window sizes · content and copy · consistency and theme.

Check actual numbers where possible: contrast ratios from the theme's color values, line heights, measure in characters, hit target px, animation durations in `.time(..)`, `animate_bool_with_time`, `Duration::from_millis`. Numbers beat vibes.

**Contrast is computed, never estimated.** Run `scripts/contrast-check.py "#fg" "#bg"` or `node scripts/contrast-check.mjs "#fg" "#bg"` (next to this file; whichever runtime exists) for every text and UI pair you judge; quote the ratio. Grey-on-grey pairs are the usual hallucination: `#555555` on black is 2.8:1 and fails. When no runtime is available, apply the WCAG formula by hand and say so.

## 2b. Look at the render

When a screenshot or a running app is available, judge the picture before the code, with the passes from `art-direction` (one second then five, squint, cover the logo, edges, space and frame, type, color, images, tells, controls, states, small window, motion, substance). Note what each pass found; those notes become findings only with evidence.

## 2c. Score (optional, for audits and before/after)

Five dimensions, 0 to 4 each, when the user wants a number or a baseline for a redesign:

| Dimension | 0 | 2 | 4 |
| --- | --- | --- | --- |
| Accessibility | unusable by keyboard or screen reader | labels exist, order or scaling breaks | labeled, ordered, scales, reduced motion honored |
| Performance felt | janky, slow launch, layout jumps | partial | instant feedback, smooth on the target device |
| Theming | hardcoded everything | tokens exist, used inconsistently | semantic throughout, both appearances first-class |
| Platform conformance | a web port, nothing native | some off-platform controls | a fluent user trusts every screen |
| Adaptivity | breaks at the minimum size | survives, awkwardly | holds at every size, density and text scale |

Report the five numbers and the sum out of 20; the findings explain every score below 3.

## 3. Report: ranked, concrete, scannable

Group findings by severity, most important first. Lead with the few that matter; do not list everything. For each finding:

- **What**: the specific issue, quoting the element or `file.rs:line` when code is available.
- **Why**: the craft or usability reason, one line.
- **Fix**: a concrete change in the project's framework and its tokens. Exact value, not "increase spacing". `.spacing(16.)` not "more gap".

Severity tiers:

- 🔴 **Blocking**: broken, keyboard-unreachable, fails WCAG AA contrast, clipped at the minimum window size, or destructive with no confirmation.
- 🟠 **Important**: noticeably hurts hierarchy, readability, usability or consistency.
- 🟡 **Polish**: refinement that sharpens the craft.

End with **Strengths** (2 to 4 things done well) and, if useful, the single highest-leverage change to make first.

## 4. Optional: apply fixes

With `--apply` (or when the user asks), edit the code for the clear, safe findings: spacing and radius values, contrast pairs via the theme, focus styles, accessible names (`a11y_alt`, `widget_info`), truncation with tooltips, hit-area minimums. Leave subjective or structural changes as recommendations unless confirmed. After editing run `cargo check` (and `cargo clippy` if the project uses it) and re-verify the values you changed. Follow the `rust-engineering` skill for any Rust you write.

## Tone

Direct and respectful, like reviewing a colleague's work: precise about problems, never vague, always paired with the fix. Calibrate depth to the surface. A launcher or onboarding screen earns motion and type scrutiny; a settings form earns states and accessibility scrutiny; a dense tool panel earns density and keyboard scrutiny.
