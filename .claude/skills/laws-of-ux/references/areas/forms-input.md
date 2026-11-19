# Forms, input, selects & touch targets

**Forms**
- Accept and normalize varied formats (phone with spaces/dashes/+country, card numbers with spaces, trimmed whitespace). A strict regex like `/^\d{10}$/` on raw input is a defect. — Postel
- Right `type`, `inputMode`, `autoComplete` (`email`, `tel`, `cc-number`, `postal-code`, `one-time-code`…); wrap in `<form>` so keyboard "Go" submits. — Postel, Parkinson
- Ask only what's needed now; infer what you can (city from postal code, card brand from number). — Tesler
- Visible labels; a placeholder is not a label. — Working Memory
- Long forms chunked into sections/steps; related fields grouped. — Chunking, Proximity
- Inline, specific errors tied to the field. Never wipe entered data on error (High). — Postel, Peak-End
- One dominant submit; Cancel/Reset quieter and not adjacent where a mis-tap destroys work. — Von Restorff, Fitts

**Long selects (countries, timezones, currencies…)**
- More than ~15 options → **searchable combobox / autocomplete is the fix**. A sensible default (locale), a placeholder option, or pinning common items on top are good *additions*, never a substitute for search. — Hick, Choice Overload
- The displayed value must match state: `value=""` with no placeholder `<option>` shows the first item while submitting empty. — Mental Model

**Targets & reach**
- iOS 44pt, Android 48dp, web ≥ 24px min (44px recommended). Pad small glyphs; clickable `<div>`/`<span>` → `<button>`/radio. — Fitts
- Space adjacent targets, especially destructive vs safe. Primary mobile actions in thumb reach (Suggestion unless the target is tiny/unreachable). — Fitts
- Hover-only interactions need touch/keyboard equivalents. — Fitts

**Code red flags (grep when reviewing source)**
`<div onClick>` · inputs without `type`/`autoComplete` · state reset in a validation-error branch · `await` in a handler with no loading/disabled/try-catch · several buttons with identical primary classes · `<select>` mapping hundreds of options · `text-red-500` as the only error signal · `defaultChecked`/`useState(true)` on consent · `<a>` without `href` used as a button.

**Code red flags, Rust native** (see `stack-map` for the APIs)
Freya: `rect().on_press(..)` with no `a11y_role`/`a11y_focusable` · `Input` with only a placeholder and no adjacent `label()` · validation branch that calls `state.set(Default::default())` · `on_press` handler that spawns a task with no `FutureState` or loading branch · several `Button`s sharing the primary theme next to each other · a `Select` or `Menu` built from hundreds of items with no filter · error shown only by `.color(theme.danger)` with no text or icon · `Switch`/`Checkbox` for consent initialized `true`.
egui: `Sense::click()` on a painted rect with no `widget_info` · `TextEdit` with `hint_text` as the only label · `ui.button("Pay")` whose handler awaits with no `spinner()` or `add_enabled(false)` · `ComboBox` listing hundreds of entries · selection state shown by `Color32` only · `bool` consent fields defaulting to `true`.
gpui: `div().on_click(..)` with no `.id`, `.track_focus` or key binding · an input view with no label element · `.bg(red())` as the only error signal · three siblings with identical primary styling.
