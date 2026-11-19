# Motion standards

Shared rulebook for `animate`, `find-animation-opportunities`, `improve-animations` and the polish domain of `better-interface`. Distilled from Emil Kowalski's animation philosophy and Apple's fluid-interface talks, rewritten in our own words and mapped to Rust toolkits by the stack map. Cite this file in findings instead of approximating.

## Should it animate at all

| Frequency | Decision |
| --- | --- |
| 100+ times a day: shortcuts, command palette, tab switch, focus moves, core navigation | Never |
| Tens a day: hover, list navigation, frequent toggles, panel resize | Remove, or near-imperceptible (≤120ms, subtle) |
| Occasional: modals, drawers, toasts, menus, settings | Standard |
| Rare: onboarding, empty states, success, celebration | Delight allowed |

Keyboard-initiated actions never animate. Data the user is reading never animates for style. A valid purpose is one of: feedback, spatial consistency, state indication, preventing a jarring change, explanation (onboarding only), delight (rare tier only).

Per form factor: desktop and web as above; mobile allows springs on gestures; TV animates focus and rails only; watch and embedded almost nothing; TUI none.

## Easing

| Case | Curve |
| --- | --- |
| Enter or exit | ease-out (fast start, settles) |
| Move or morph on screen | ease-in-out |
| Hover or color change | ease |
| Continuous (marquee, progress, spinner) | linear |
| Default when unsure | ease-out |

Prefer exponential ease-out families (quart, quint, expo). Never ease-in on an entrance. Never bounce or elastic on functional UI; reserve for the delight tier or gesture settle. In Rust: Freya `Ease::Out` + `Function::Expo` (or `Quart`), egui `animate_bool_with_easing`, gpui `ease_out_quint()`, Slint `easing: ease-out` or `cubic-bezier(0.23, 1, 0.32, 1)`.

## Duration

| Element | ms |
| --- | --- |
| Press feedback | 100–160 |
| Tooltip, small popover | 125–200 |
| Dropdown, select, menu | 150–250 |
| Modal, drawer, sheet | 200–500 |
| Page or screen transition | 200–350 |
| Marketing, onboarding | longer, authored |

UI under 300ms. Exit at about 70% of the enter duration. Desktop at the low end, mobile mid, TV focus 150–250, embedded ≤150.

## Properties

Animate opacity, scale, translation, color, and clip. Avoid animating layout-driving width and height of large containers every frame unless the toolkit's layout is cheap for that subtree; on egui and immediate-mode toolkits, animate a value and derive the layout from it once per frame. Never animate blur or shadow on weak GPUs or embedded.

## Origin and direction

Popovers, menus and selects scale from the edge of their trigger (transform origin at the anchor). Modals stay centered. Toasts and sheets exit the way they entered (same edge, same distance). Lists enter with a small offset (4–8px) and opacity, never from scale 0.

## Interruptible

A state change that can reverse mid-way must retarget from the current value, not restart from the start: Freya `.reverse()` on a running `AnimNum`, egui per-frame `animate_bool` targets, gpui restart with a short duration, Slint `animate` on a property retargets automatically. One-shot sequences (staggers, celebrations) are the only place a non-interruptible timeline is acceptable.

## Springs and gestures (fluid interfaces)

Motion starts from the current on-screen value, inherits the user's velocity, projects momentum forward, resists at boundaries (rubber-band), and can be grabbed and redirected at any instant. Dismiss on velocity (about 0.11 px/ms) rather than distance alone. Springs: duration 300–500ms, bounce 0.1–0.3 on gestures, 0 on UI state changes. Where the toolkit has no spring (Freya, egui, Slint), approximate with `Function::Back`/overshoot curves kept under 10% overshoot, or ease-out with no bounce.

## Stagger

Only for infrequent group entrances the user sees occasionally: 30–80ms between items, capped at about 10 items, never blocking input, never on lists that refresh often.

## First render

Entrances do not fire for state that already exists when the screen mounts. Seed the animated value with its final state on mount (Freya start policy, egui seeded `animate_bool`, gpui a mounted flag, Slint initial property value).

## Reduced motion

The toolkits do not read the OS setting. The project owns one `reduce_motion` setting; every animation reads it: slides and scales become crossfades, parallax and autoplay stop, durations do not go to zero (an instant change can be more jarring than a 100ms fade).

## Performance

Measure on the slowest target device, never the dev machine. 60Hz gives 16ms per frame, 120Hz gives 8ms. Immediate-mode toolkits repaint on demand: request a repaint only while something animates. Retained toolkits: avoid relayout of large subtrees per frame. Embedded: no per-frame allocation, pre-rasterized assets.

## Review checklist

1. Should it animate (frequency tier, purpose named)?
2. Right curve for the case?
3. Duration inside the budget for the element and the form factor?
4. Properties cheap on this toolkit?
5. Origin at the trigger, symmetric exit?
6. Interruptible, no first-render fire?
7. Reduce-motion respected?
8. Measured on the target device?
