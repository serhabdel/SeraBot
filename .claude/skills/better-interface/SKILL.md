---
name: better-interface
description: 'Cross-discipline review of a UI in any stack (Rust desktop with Freya, egui or gpui first-class; web and mobile too) as one system. Walks accessibility, layout, writing, typography and UI polish in that order, cites file:line evidence from the Rust source, and consolidates one ranked verdict (Block or Approve). Use when the user asks to review, critique or audit a screen, flow, component or whole app in Rust, web or mobile, or asks whether the interface is ready to ship. Adapted for local Rust code from Jakub Krehel''s better-interface skill. Usually invoked by the definitive-ux router; can be called directly.'
---

# Review the interface as one system

> **Stack:** Rust native crates load the `stack-map` skill for every API name. Web and mobile projects use `stack-map/maps/web.md` and the original web idioms. The rules below are stack-agnostic; only the fixes change idiom. Severity maps to the unified scale in the `definitive-ux` orchestrator when run through it.

Adapted from Jakub Krehel's `better-interface` (jakub.jp). The rulebook is his. The inspection surface is Rust source and a running native window instead of DOM and CSS.

A strong interface is not five independent audits stapled together. Position and spacing carry hierarchy before a word is read, the words do the explaining, type makes them legible, and polish makes the result feel deliberate. Review the whole experience, then consolidate into one prioritized verdict.

## How to run a review

### 0. Load the framework map

Load the `stack-map` skill first. It tells you which framework the crate uses and where every concept below lives in that framework's API. Every fix in this review is written in that framework's idiom. Never propose CSS, HTML or a different Rust toolkit.

### 1. Resolve scope and mode

Infer the screen, flow, component or crate from the request and the workspace. State the resolved scope in the output. Default mode is `full`.

| Mode | Coverage | Finding cap |
| --- | --- | --- |
| `quick` | Primary path and highest-traffic states; `HIGH` and `MEDIUM` only | 5 |
| `full` | Entire scope across all five domains, including empty, loading, error and small-window states where they exist | 15 |

If the scope is too large to inspect credibly, narrow it to the one complete flow the request centers on, or the entry path every user crosses. State the boundary. Never imply uninspected surfaces were reviewed.

A request naming a branch, commit range or uncommitted diff is a change review. Review only what the diff touches, label each finding `Introduced`, `Regression` or `Pre-existing`, and let the verdict cover the first two only.

### 2. Recon before judgment

From `stack-map`: framework, theme or token module, component crate, supported window sizes, test and preview commands. Index the crate with `codebase-memory-mcp` when that MCP server is available (otherwise use the host's symbol search) (`list_projects`, `index_repository`) so you can ask who renders a component and how far a shared fix reaches. Read `CLAUDE.md`, `AGENTS.md`, a design or theme doc. Name what you found, or that there is none. A documented convention settles taste. It never excuses a keyboard, focus or contrast failure.

### 3. Review in this order

Foundational failures must not be hidden by polish:

1. Accessibility
2. Layout
3. Writing
4. Typography
5. UI polish

When two domains cover one issue, assign it to the one that owns the rule and mention the side effect in **Why**. Report it once.

### 4. Require evidence

Every finding cites `path/to/file.rs:line` and shows the current code. A rendering claim (contrast, overlap, clipping, motion feel) needs a render: launch the app with the `run` skill and screenshot, use `freya-testing` `render_to_file`, `egui_kittest`, or ask the user for a screenshot and window size. Source alone cannot support a visual finding; a screenshot alone cannot support a code finding. What you could not render is **Not verified**, never a finding.

### 5. Review without mutating

Read-only unless the user also asks you to implement. When they do, keep the consolidated report as the change scope, apply fixes in the project's idiom, run `cargo check` and the relevant test, and re-run the verification.

## Accessibility

Native desktop toolkits do not give accessibility for free the way HTML does. The floor is still the same: every control reachable and operable by keyboard, every control named, focus always visible, nothing carried by color alone.

### 1. Built-in components first

Use the toolkit's own `Button`, `Switch`, `Checkbox`, `Input`, `Select`, `Menu`, `Popup` before a hand-rolled `rect()` with `on_press`, a `Sense::click()` rectangle or a bare `div().on_click`. The built-ins already carry role, focus and keyboard handling. A custom control is a finding unless it reproduces all three.

### 2. Visible focus

Keyboard focus needs a visible indicator that contrasts against every surface it crosses. Freya: a style branch on `focus.is_focused()`. egui: keep the default focus stroke; a `Visuals` override that removes it without a replacement is `HIGH`. gpui: `.focus_visible(..)` or `.in_focus(..)` on the element that owns the `FocusHandle`.

### 3. Full keyboard support

Every pointer interaction needs a keyboard path. Escape closes overlays, arrow keys move within tabs, menus and lists, Tab moves between widgets, Enter and Space activate. Freya: `.a11y_focusable(true)` plus `.on_key_down`; egui: `key_pressed` handling and `FocusDirection`; gpui: `actions!` plus `bind_keys` with a `key_context`. A control reachable by pointer but not by keyboard is `HIGH` on sight.

### 4. Trap and restore focus

Modals move focus inside on open and return it to the trigger on close. Background content must not receive keys or scroll while a modal is up. Check custom overlays, not only the toolkit's `Popup` or `Modal`.

### 5. Minimum hit area

Baseline 24×24 logical px; aim for 40×40 on desktop when density permits and 44×44 for touch targets. Check `min_width`/`min_height`, `interact_size`, `.min_w_*`. Extended hit areas must not overlap.

### 6. Label and type every control

Every input has a visible label and the toolkit knows the association: Freya `Input` with adjacent `label()` inside one focusable group, egui `response.labelled_by(id)`, gpui a label element next to the input inside the same focus group. A placeholder is never a label. Never block paste.

### 7. Accessible names everywhere

Icon-only buttons need a name the screen reader can speak: Freya `.a11y_role(AccessibilityRole::Button)` plus `.a11y_alt("Close")`, egui `widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, "Close"))`. Decorative `svg()` and `image()` must not be focusable. Where the toolkit cannot expose a name (gpui today), report it once as a platform limitation and mark the domain partially **Not verified**.

### 8. Don't rely on color alone

Status needs a redundant cue: icon, text or shape next to the color. Measure the rendered foreground and background pair against WCAG: 4.5:1 for body text, 3:1 for large text and UI glyphs. Report the pair and the requirement it misses; do not repaint the theme unless asked.

### 9. Honor reduced motion

The toolkits do not read the OS setting. The project should own a `reduce_motion` setting (config, env or theme flag) and every `use_animation`, `animate_bool`, `with_animation` should respect it. If no such setting exists, report it once as a `MEDIUM` capability gap. Motion that ignores an existing setting is `HIGH`. Autoplaying content needs a visible pause; toasts carrying actions or errors stay until dismissed.

## Layout

Layout communicates before a single word is read. A good layout survives stress: resize the window to its minimum, lengthen every string, and it should still hold.

### 1. Group with space, not lines

Negative space first, background surfaces second, separators last. The gap between groups is at least 2× the gap within a group (`.spacing(8.)` inside means `16.`+ between groups). A `separator()` or 1px border where spacing would carry the structure is a finding whose fix is deletion.

### 2. Keep controls distinct from content

Interactive elements look interactive: a background, a border or a consistent placement zone. A clickable `label()` styled like adjacent static text is a finding.

### 3. Align to shared edges

Pick alignment edges and keep them. One spacing step per level of subordination (16 is a sane default). Stray `padding((10., 24.))` or `.px_3()` beside `.px_4()` siblings reads as noise.

### 4. Order by importance

Most important content near the top and leading edge. Reading order top-to-bottom, leading-to-trailing.

### 5. Breathing room between targets

Start from 12px between adjacent bordered or filled controls and 24px of clearance around borderless icon buttons. Compact tool UIs may use less when hit areas do not overlap and controls stay distinct.

### 6. Hold structure until it breaks

Breakpoints come from content, not device presets. Keep the expanded layout as long as it fits and collapse late. Test the minimum window size the app allows (`WindowConfig`, `NativeOptions::min_window_size`, `WindowOptions`) and the largest.

### 7. Plan for growth and clipping

No fixed widths or heights on text containers. Let rows wrap. Never park critical actions where a resize clips them. Check `.overflow(..)` and `.overflow_hidden()` on anything holding text or buttons.

## Writing

Clear and brief beats clever. Consistency beats variety. The best error message is the interaction redesigned so the error cannot happen. Strings live in `.ftl` files (freya-i18n), `const` tables or inline literals; review the text wherever it lives.

### 1. One voice, flexible tone

If it is "Archive" in the menu it is not "Move to storage" in the toast. Tone flexes with stakes: warm for success and empty states, neutral for settings, calm and plain for errors and destructive confirmations, serious for data loss.

### 2. Plain words over clever ones

Delete every word that is not needed. No idioms or humor that will not translate. Never concatenate fragments around variables (`format!("You have {} new", n)` with a hand-pluralized suffix); use full templated strings with pluralization (`t!("messages", count: n)` in Fluent).

### 3. Verb-first buttons

"Send", "Save draft", "Delete project". Confirmation dialogs repeat the consequence: `Delete project` and `Cancel`, never `Yes` and `No`.

### 4. Links describe their destination

"Read the billing docs", never "Click here" or a bare "Learn more" repeated across a screen.

### 5. One capitalization policy

Sentence case is the safer default. "Save Changes" beside "Discard changes" reads as sloppiness.

### 6. Errors say how to fix, next to where it broke

| Bad | Good |
| --- | --- |
| That password is too short | Choose a password with at least 8 characters |
| Invalid name | Use only letters for your name |
| Oops! Something went wrong. | Unable to save. Check your connection and try again. |

No blame, no "oops", no exclamation marks. An error with no recovery path is `HIGH`.

### 7. Empty states point forward

Say what this place is and how to fill it, with one clear next action. Search and filter empties name the query and offer an exit: "No results for 'quarterly'. Clear filters".

## Typography

Mostly restraint. A sensible scale, comfortable spacing and enough contrast beat any effect.

### 1. Fewer fonts, sizes and weights

Rarely more than two families. Below 18px stay at regular weight or heavier; thin weights are display-only.

### 2. Use a type scale with semantic names

A small set of sizes in one place: a `typography` module, egui `text_styles`, gpui `text_sm`/`text_base`/`text_lg`. Scattered `.font_size(15.)`, `.font_size(17.)` literals are one systemic finding against the missing scale.

### 3. Heading sizes descend with level

A subordinate heading never overpowers its parent. Adjacent levels may share a size near the small end if weight or spacing separates them.

### 4. Line height by role

Headings around 1.1, body 1.5 to 1.6. Anything wrapping to three or more lines needs at least 1.4. Freya `.line_height(1.5)`, gpui `.line_height(relative(1.5))`, egui row spacing via `item_spacing.y`.

### 5. Cap the measure

Long-form text at 60 to 75 characters per line. Cap with `.max_width(Size::px(640.))`, `ui.set_max_width(640.)`, `.max_w_2xl()`.

### 6. Wrap deliberately

Labels and badges never wrap: `.max_lines(1)`, `TextWrapMode::Extend` or `Truncate`, `.whitespace_nowrap()`. Long IDs and paths must be allowed to break or truncate with the full value reachable.

### 7. Tabular numbers on changing values

Timers, counters and prices shift layout as they update. Use a monospace or tabular-figure family for them.

### 8. Truncate without losing content

`.max_lines(1).text_overflow(TextOverflow::Ellipsis)`, `Label::truncate()`, `.truncate()`. Truncated content that matters must be reachable in a tooltip or expanded view. Truncation with no way to the full value is `HIGH`.

## UI polish

Great interfaces come from small details that compound. This section assumes an animation belongs. Whether it belongs at all is `find-animation-opportunities`' question.

### 1. Concentric border radius

Outer radius = inner radius + padding. `corner_radius(12.)` wrapping a `corner_radius(12.)` child with `padding(8.)` is the most common thing that makes a UI feel off.

### 2. Optical over geometric alignment

Icon beside text, play triangles and asymmetric glyphs need manual nudges. Check `svg()` and `img()` inside buttons.

### 3. Shadows for elevation, borders for structure

A border whose only job is depth becomes a soft shadow (`.shadow(..)`, `Frame::shadow`, `.shadow_sm()`). Keep borders that communicate structure or state.

### 4. Interruptible animations

Prefer retargetable primitives for state changes: Freya `AnimNum` with `.reverse()`, egui `animate_bool`. One-shot sequences (`AnimSequential`, gpui `.repeat()` loops) are for staged entrances only.

### 5. Split and stagger enter animations

Only for infrequent staged entrances. Stagger by about 100ms. Anything the user triggers often never staggers.

### 6. Subtle exit animations

Exits softer than enters: a small fixed offset, not full height. Ease-out on both.

### 7. Contextual icon animations

Swap icons with opacity and scale (0.25 to 1) rather than toggling visibility. Freya `.opacity(..)`/`.scale(..)` driven by one `AnimNum`; gpui `with_animation` on opacity.

### 8. Image outlines

A 1px low-opacity outline on images: pure black at 10% in light themes, pure white at 10% in dark. Never a tinted neutral.

### 9. Scale on press

A subtle `0.96` scale on press, never below `0.95`. Freya `.scale(0.96)` from a pressed state; egui `WidgetVisuals::expansion` on `active`; gpui has no scale transform on `Styled`, so use `.active(|s| s.opacity(0.9))` or a background shift.

### 10. Skip animation on first render

Entrances must not fire for state that already exists on mount. Freya start policy `OnCreation` vs `OnChange`; egui seed `animate_bool` with the initial value on the first frame; gpui gate `with_animation` behind a "mounted" flag.

### 11. Animate the right properties

Opacity, scale, offset and color. Never animate layout-driving width and height of large containers every frame unless the toolkit's layout engine is cheap for that subtree. Check `AnimNum` driving `.width(Size::px(..))` on big lists.

### 12. Match icon stroke to text weight

1.5px stroke beside regular text, 2px beside semibold. One icon set per surface (`freya-icons` Lucide, one SVG pack).

### 13. One SVG, recolored per state

Icons take `.color(..)` / `.text_color(..)` from the theme and get hover, selected and disabled states from color and opacity, never from separate assets. Outline is the default, fill marks the active state.

## Severity

- `HIGH`: blocks a task, misleads the user, hides content or controls, risks data loss, or is a repeated systemic failure.
- `MEDIUM`: meaningfully harms comprehension, efficiency, adaptability or consistency.
- `LOW`: isolated polish. `full` mode only.

Escalation triggers are `HIGH` on sight: no accessible name, no visible focus, pointer-only control, motion ignoring an existing reduce-motion setting, content clipped at the minimum window size, failing contrast, meaning by color alone, destructive action with no confirmation or undo, truncation with no path to the full value, error with no recovery, danger color on a non-destructive action, state carried by motion alone.

Within a severity, rank by reach. A token or shared-component fix outranks the same symptom in one leaf. Use the `codebase-memory-mcp` graph to count call sites.

### Prefer the cheaper fix

1. Delete (the separator, the animation on a frequent action, the redundant wrapper).
2. Use the toolkit (built-in component, default focus ring).
3. Reuse what the project has (existing token, spacing step, easing).
4. Correct the value.
5. Add (new token, new setting, new wrapper).

## Review output format

### Scope and coverage

State mode, exact scope, framework and version, theme or token module, convention docs found, and any boundary. Then:

| Domain | Evidence inspected | Result |
| --- | --- | --- |
| Accessibility | files, components, states or checks | count or `Clear` |

All five domains. `Clear` means inspected with no finding. `Not reviewed` must say why.

### Findings

| # | Severity | Domain | Location | Before | After | Why |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | HIGH | Accessibility | `src/ui/toolbar.rs:42` | `rect().on_press(close).child(svg(icons::lucide::x()))` | `Button::new().a11y_alt("Close").on_press(close).child(..)` or add `.a11y_role(AccessibilityRole::Button).a11y_alt("Close").a11y_focusable(true)` | Icon-only control has no accessible name and is not keyboard reachable |

One row per root cause with every confirmed location. Respect the cap; never pad. With no findings, omit the table and state "No actionable interface findings."

### Verification

List each check: command or interaction, observed result. Separate passed checks from **Not verified**.

### Verdict

- `Block`: one or more `HIGH` findings remain.
- `Approve`: no `HIGH` findings remain. `MEDIUM` and `LOW` stay in the table as work to do.

`Approve` covers only the domains the coverage table shows as inspected. Name every `Not reviewed` domain in the verdict line.
