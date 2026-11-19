# Web (browsers)

Any page or app rendered by a browser: plain HTML/CSS, React, Vue, Svelte, Astro, Leptos, Yew, Dioxus web, and the content layer of Tauri, Dioxus desktop and Electron. egui and Freya on wasm render to a canvas and get this file's rules with reduced accessibility (say so).

## Conventions to check

- **Native elements first**: button, a[href], input, select, dialog, details; never div with onClick. Semantic landmarks and heading order.
- **Visible focus** with `:focus-visible`, 2px minimum, never `outline: none` without a replacement. Full keyboard operability, roving tabindex in composite widgets, Escape closes.
- **Accessible names**: aria-label on icon buttons, label-for on inputs, alt on images, aria-hidden on decoration. ARIA only where native cannot.
- **Reduced motion** opt-in via `prefers-reduced-motion: no-preference`.
- **Hit targets** 24px minimum, 44px recommended on touch.
- **Responsive from 320px to 1440px+**, container queries for components, no horizontal scroll, no fixed widths on text, inputs at 16px on mobile.
- **Contrast** computed from computed colors; 4.5:1 body, 3:1 large and UI.
- **Performance felt as UX**: no layout shift (reserve space), lazy images with dimensions, fonts with `font-display: swap`, interaction response under 100ms.
- **Browser surfaces are part of the design**: selection color, caret, scrollbar, focus ring, underline offset, tabular numerals; defaults read as "assembled".
- **Dark mode** via `prefers-color-scheme` or a `data-theme` attribute with tokens on `:root`.
- **Forms**: right `type`, `inputmode`, `autocomplete`, errors adjacent and announced, never block paste, submit on Enter.

## Registers

A marketing page, landing, portfolio or campaign is **persuade**: the design is the product; art direction leads (see `art-direction`). A dashboard, admin, tool or settings surface is **operate**: the design serves the task; density, keyboard and states lead. Documentation, articles and changelogs are **read**: the reading column stays calm; the frame carries the identity.

## Motion

CSS transitions for state changes, keyframes for one-shot sequences, transform and opacity, `@starting-style` for entrances, exponential ease-out; one authored moment per page rather than a fade on every section.
