<!-- verified: 2026-10-23, idioms from the original web-authored skills (better-interface, design-review, Superfuture, Emil Kowalski); stable web platform APIs -->
# Web and mobile map

Used when the stack is not Rust native. The specialist rules were written for the web originally, so most idioms are direct. This file lists the ones the Rust-adapted skills translated, so they can be translated back.

## Web (HTML, CSS, React, Vue, Svelte, Tailwind)

| Concept | Idiom |
| --- | --- |
| Native control first | `<button>`, `<a href>`, `<input>`, `<select>`, `<dialog>`; never `<div onClick>` |
| Visible focus | `:focus-visible` ring, 2px minimum, never `outline: none` without replacement |
| Keyboard | Tab order, roving `tabindex` in composite widgets, Escape closes, Enter and Space activate |
| Accessible name | `aria-label` on icon buttons, `<label for>` on inputs, `alt` on images, `aria-hidden` on decoration |
| Reduced motion | `@media (prefers-reduced-motion: no-preference)` makes motion opt-in |
| Hit area | 24×24 CSS px minimum (WCAG 2.5.8), 44×44 recommended; pad with a pseudo-element |
| Spacing scale | Tailwind `p-1` 4px … `p-4` 16px … `p-6` 24px; arbitrary `p-[14px]` is off-grid |
| Type scale | `text-sm` 14px, `text-base` 16px, `text-lg` 18px; `leading-relaxed` 1.625; `max-w-prose` 65ch |
| Truncation | `truncate`, `line-clamp-n`, `overflow-wrap: anywhere` for IDs |
| Tabular numbers | `tabular-nums` |
| Motion | CSS transitions for state changes (interruptible), keyframes for one-shot sequences; animate `transform` and `opacity` only; `@starting-style` for entrances |
| Press feedback | `:active { transform: scale(0.96) }`, `transition: transform 160ms ease-out` |
| Easing tokens | `--ease-out: cubic-bezier(0.23, 1, 0.32, 1)`, `--ease-in-out: cubic-bezier(0.77, 0, 0.175, 1)` |
| States | `:hover` gated by `@media (hover: hover) and (pointer: fine)`, `:disabled`, `aria-busy` for loading |
| Responsive | container queries for components, breakpoints from content, test 320px and 1440px |
| Contrast | compute from computed colors; 4.5:1 body, 3:1 large and UI |
| Theme | CSS custom properties on `:root`, dark via `prefers-color-scheme` or a `data-theme` attribute |

Code red flags to grep: `<div onClick`, `onClick` on `<span>`, `<a>` without `href`, inputs without `type`/`autoComplete`, `outline: none`, `transition: all`, state reset inside a validation error branch, `await` in a handler with no loading or disabled state, `<select>` mapping hundreds of options, color-only error signals, `defaultChecked` on consent.

## Mobile (SwiftUI, Compose, Flutter, React Native)

| Concept | Idiom |
| --- | --- |
| Hit area | 44pt iOS, 48dp Android |
| Primary action | bottom of the screen, thumb reach; never a small top-right text button for the main step |
| Accessible name | `accessibilityLabel`, `contentDescription`, `Semantics(label:)` |
| Reduced motion | `UIAccessibility.isReduceMotionEnabled`, `Settings.Global.ANIMATOR_DURATION_SCALE`, `MediaQuery.disableAnimations` |
| Input types | keyboard type and content type per field (email, phone, one-time code) |
| States | loading, empty, error, offline, permission denied |
| Progress | step indicator in multi-step flows; head start on the first step |

Thresholds for all stacks live in `laws-of-ux`: targets, response times, memory chunks, measure, contrast.
