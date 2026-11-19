# Playbooks

Each playbook is a sequence. Read-only steps never touch source. Steps marked **confirm** need the user's go-ahead before edits. Every playbook ends by merging into the report skeleton in `SKILL.md`.

## P1. New native app from zero (Rust)

Trigger: "quiero hacer una app de escritorio", "nuevo proyecto freya/egui/gpui", "arma la UI de…".

1. `stack-map`: detect or choose the framework; state the idioms the build will use.
2. `map-user-flows` in **plan mode**: with no code yet, derive screens, phases and stories from the description. Output becomes the build plan. Ask the entry point and platform if ambiguous.
3. `laws-of-ux` build rules: one primary action per view, chunking, defaults, states, targets, progress. Decide before coding.
4. Build with `rust-engineering` (and `freya` when applicable). Theme tokens first, components second, screens third. Every screen gets loading, empty and error branches from day one.
5. `spacing-audit` on the new code, summary only, then snap.
6. `better-interface` quick mode on the entry flow.
7. `find-animation-opportunities` in reject-heavy posture; implement at most the top one if the user wants motion.
8. Report: the built artifact, a "UX decisions" note, the quick review findings.

## P2. Company website or landing page (web)

Trigger: "quiero hacer una web para mi empresa", "landing", "sitio".

1. Stack is web. Load `../stack-map/maps/web.md`.
2. `laws-of-ux` build rules plus `areas/navigation-choices.md` and `areas/layout-content.md`. Decide: one dominant CTA per view, conventional nav (Jakob), scannable sections (chunking, serial position), real content hierarchy.
3. If an aesthetics skill is installed (`impeccable`, `frontend-design`, `design-taste-frontend`, `high-end-visual-design`, `gpt-taste`), run it for visual direction. This plugin does not pick fonts or palettes.
4. Build. Semantic HTML, visible focus, labels, `prefers-reduced-motion`, responsive from 320px.
5. `design-review` on the result with `laws-of-ux` areas loaded. Blocking items fixed before delivery.
6. `find-animation-opportunities`: marketing pages are where the delight budget lives, but hero and nav still obey the gate.
7. Report: artifact, UX decisions note, review findings.

## P3. Pricing page

Trigger: "página de precios", "planes", "pricing".

Pricing is a **product persuade** surface. Run **P20** with `art-direction` in restraint mode (gate and direction sheet still required; the direction will usually be convention plus two or three decided details and the product's own fragment as the only image) with these constraints carried as the floor:

1. `laws-of-ux` `areas/navigation-choices.md` pricing block: 3 to 4 self-serve tiers in the row, enterprise separated, one recommended plan (badge plus structure, never color alone), differentiators only on cards, the full matrix in a comparison table below.
2. `laws-of-ux` ethics: no fake scarcity, no confirmshaming, cancel and downgrade stated plainly, trial terms explicit.
3. `design-review` on the result: Von Restorff on the recommended plan, Similarity across cards, hit areas on the period toggle, contrast computed.

A pricing page built from the three constraints alone, with the typeface, accent and spacing stated in the sheet, is a correct answer. A pricing page dressed as an invoice, with stamps, three fonts and an estimator the product lacks, fails the build even though every rule was followed.
## P4. Redesign an existing screen or app

Trigger: "rediseñar", "modernizar", "se ve viejo", "hazlo premium", "mejora la UI".

1. Detect stack. Rust: `stack-map` and index with `codebase-memory-mcp`.
2. `analyze-components` read-only: inventory, duplicates, hardcoded tokens. The redesign must not multiply components.
3. `design-review` on the current state as the **baseline**. Save it; it is the before.
4. `map-user-flows` if the redesign touches more than one screen, to catch missing states before restyling.
5. Plan: list the findings the redesign will resolve, the tokens it will introduce, the components it will consolidate. **confirm**.
6. Implement with `rust-engineering` or the web aesthetics skill. Tokens first.
7. `spacing-audit` summary then snap.
8. `better-interface` full on the result. Compare against the baseline; every baseline Blocking must be gone.
9. `find-animation-opportunities` only if the user asked for "feel"; otherwise skip.
10. Report with a before/after table of resolved findings.

## P5. Pre-ship audit

Trigger: "antes de lanzar", "is it ready", "auditoría", "release".

1. Stack detection and index.
2. `better-interface` full, all five domains, with `laws-of-ux` areas `feedback-states`, `forms-input`, `ethics` loaded for Why cells.
3. `spacing-audit` summary only (no edits); its systemic finding becomes one row.
4. `design-review` contrast numbers (§4) if the theme has not been measured.
5. Verdict `Block` or `Approve`. Blocking list first. Offer `--apply` for the safe ones.

## P6. Change review (branch, PR, diff)

Trigger: a branch name, PR number, "lo que cambié", "revisa mi diff".

1. Scope = the diff only. Index or `detect_changes`.
2. `better-interface` change-scoped: findings tagged Introduced / Regression / Pre-existing.
3. `spacing-audit` on changed files only.
4. If the diff adds animation: `find-animation-opportunities` gate on each new animation (frequency first).
5. Verdict covers Introduced and Regression only.

## P7. Motion pass

Trigger: "animar", "más vivo", "se siente muerto", "feel".

1. `stack-map` motion row for the framework, `definitive-ux/references/motion-standards.md` for the rules.
2. `find-animation-opportunities` read-only. Rejected list is mandatory.
3. If the user wants implementation: take the top 1 to 3 rows, implement with `rust-engineering`, respect an existing reduce-motion setting or add one once.
4. `better-interface` polish domain on the implemented motion (interruptible, no first-render fire, right properties).

## P8. Spacing cleanup

Trigger: "espaciado", "desalineado", "4/8", "ritmo".

1. `stack-map` spacing row.
2. `spacing-audit` summary. **confirm**. Snap. `cargo check`.
3. If the audit finds no scale module, propose one token module as a single Important finding; do not create it unasked.

## P9. Component cleanup / design system

Trigger: "limpiar componentes", "duplicados", "design system", "inventario".

1. Index with `codebase-memory-mcp`.
2. `analyze-components`: writes `docs/component-audit.md`. Read-only.
3. **confirm** the fix plan. Apply in order: delete orphans, consolidate duplicates, extract tokens, add missing states, rename.
4. `better-interface` quick on the consolidated components.

## P10. PRD, user stories, flow map from code

Trigger: "user stories", "mapa de flujos", "PRD", "qué pantallas faltan", "handoff".

1. Index. `stack-map` navigation row to find routes.
2. `map-user-flows`: writes `docs/user-flows.md` with Mermaid and action items. Read-only.
3. If the user then wants the gaps built, each action item carries a paste-ready prompt; run P1 steps 3 to 6 per gap.

## P11. Onboarding

Trigger: "onboarding", "primer uso", "tutorial", "registro".

1. `laws-of-ux` `areas/onboarding-flows.md`: value before effort, defer fields, progress, head start, finish strong.
2. `map-user-flows` on the onboarding screens to catch dead ends.
3. Build or review per intent; `better-interface` writing domain on the copy.

## P12. Forms and checkout

Trigger: "formulario", "checkout", "pago", "validación", "abandono".

1. `laws-of-ux` `areas/forms-input.md` including the code red flags for the stack.
2. `better-interface` accessibility and writing domains (labels, errors, keyboard).
3. `design-review` §6 states: loading on submit, never wipe data on error, one dominant submit.
4. `laws-of-ux` ethics on defaults and consent.

## P13. Dashboard or data-dense tool

Trigger: "dashboard", "tabla", "métricas", "editor", "panel".

1. `laws-of-ux` Pareto and cognitive load: surface the few metrics, the rest on demand.
2. `better-interface` with density allowance: compact spacing is fine when hit areas do not overlap.
3. `find-animation-opportunities` in reject mode: data the user reads never animates for style.
4. Typography: tabular numbers, truncation with full value reachable.

## P14. Accessibility pass

Trigger: "accesibilidad", "teclado", "screen reader", "contraste", "a11y".

1. `stack-map` keyboard/focus/a11y rows (what the toolkit can and cannot expose).
2. `better-interface` accessibility domain only, every trigger is Blocking.
3. `design-review` §4 contrast numbers for both theme variants.
4. Where the toolkit cannot expose a name or role (gpui today), one Important finding naming the platform gap, not one per element.

## P15. Theme, dark mode, tokens

Trigger: "tema", "dark mode", "modo oscuro", "tokens", "colores".

1. `analyze-components` hardcoded-style detection only.
2. `design-review` §4 and §10: contrast in both modes, consistent radii and elevation.
3. `better-interface` polish: image outlines per mode, icon recoloring from `currentColor` equivalents.

## P16. Internationalization

Trigger: "traducir", "i18n", "idiomas", "RTL".

1. `better-interface` writing domain: no concatenated fragments, pluralization, one voice.
2. `better-interface` layout: growth and clipping with long strings; leading/trailing not left/right.
3. For Freya: `freya-i18n` and `.ftl`; the review reads the strings there.

## P17. Quick question

Trigger: one specific doubt, a threshold, "which law applies".

1. `laws-of-ux` quick mode only. Under 200 words, one law at most, answer then reason then fix.
2. No other skill runs. No report skeleton.

## P18. Visual-only review (screenshot, Figma, Penpot)

Trigger: an image or design link and "qué opinas", "revisa".

1. `design-review` from the render. Ask the viewport and theme.
2. `laws-of-ux` areas matching the surface for Why.
3. Code-level domains (keyboard, names, states not visible) marked Not verified. Offer to review the source.

## Combining rules

- One full review per scope per turn.
- Inventory and flows before restyling. Never redesign what you have not mapped.
- Spacing and motion after structure. Never polish a layout that will change.
- Build intents always end with a review loop on the artifact, quick depth unless the user asked for full.
- Every edit step is preceded by the summary it is based on and, where marked, by confirmation.

## P19. Design source: tokens, theme, cross-platform consistency

Trigger: "tokens", "theme", "dark mode", "DESIGN.md", "que se vea igual en desktop y móvil", "design system".

1. `stack-map` for every target in the workspace.
2. `design-source` in **document** mode if code exists, **create** otherwise. Read-only until the files are proposed. **confirm**.
3. `design-source` **derive** for each target; `analyze-components` lists the components still reading literals; **extract** after confirmation.
4. `design-review` §4 contrast in both modes on the derived themes.

## P20. Persuade surface: landing, marketing, launch, store page

Trigger: "landing", "web para mi empresa", "página de producto", "portfolio", "se ve genérica", "AI-made". First decide product persuade (restraint mode) or brand persuade (expressive mode); a company site for a service business is usually product persuade with one brand moment in the hero.

1. Stack and form factor (web most often; a native showcase screen sometimes).
2. `design-source` if the product has no tokens yet (the direction's palette and type become the semantic layer).
3. `art-direction` gate: `design/direction.md` with inventory, argument, three directions, the chosen one and the sheet, written before any markup. **confirm** the chosen direction in one line when the brief is vague; otherwise state it and continue.
4. Build in the stack's idiom.
5. `art-direction` look-and-fix passes on the render, or in degraded form from the markup when nothing can render (say so); then `design-review` for Blocking items.
6. `find-animation-opportunities` for the one signature moment.

Replaces P2 for web sites; P2 steps 3 to 6 now live here.

## P21. Bolder / quieter / distill

Trigger: "más audaz", "bájale", "simplificá".

- Bolder: `art-direction` direction pass (idea, color strategy one step up: restrained → committed → full), then `design-review`. Never add tells to add energy.
- Quieter: `design-review` with `slop-tells.md`, `spacing-audit`, reduce the color strategy one step, remove motion that fails the gate.
- Distill: `map-user-flows` to find what the task needs, `laws-of-ux` Hick/Occam/Tesler, then `better-interface` quick on the reduced surface. Removed options go behind progressive disclosure, never deleted if the system cannot decide for the user.

## P22. Harden before production

Trigger: "producción", "edge cases", "qué pasa si", "i18n", "offline".

1. `break-ui` on the main components and screens.
2. `better-interface` writing (errors with recovery, pluralization), layout (growth and clipping), accessibility.
3. `map-user-flows` design-needs section for missing error, empty, offline and permission states.
4. Platform file: offline and degraded states for embedded and mobile.

## P23. Motion, full

Trigger: "animaciones", "que se sienta fluido", "springs", "transiciones".

1. `stack-map` motion row + `motion-standards.md`.
2. `improve-animations` on what exists (read-only plans).
3. `find-animation-opportunities` for what is missing (reject list mandatory).
4. **confirm** which plans and rows to implement; `animate` builds each one; `rust-engineering` for Rust edits; `cargo check`.
5. Per form factor: TV animates focus and rails only, watch and embedded almost nothing, TUI none.

## P24. Native feel for a new form factor

Trigger: "llevarlo a TV", "versión para reloj", "HMI embebido", "que se sienta nativo en macOS".

1. `stack-map` for the target framework; `platforms/<form factor>.md` whole.
2. `design-source` derive for the target (density, type sizes, motion tokens change).
3. `map-user-flows` on the existing app to decide which flows survive on the new form factor (watch keeps one or two).
4. Build; `better-interface` with the platform file leading; `break-ui` at the form factor's worst case (TV: 10-foot text; watch: round clipping; embedded: exact pixels).

## Combining rules (amended)

- One full review per scope per turn.
- Inventory and flows before restyling. Design source before building. Direction before markup on persuade surfaces.
- Spacing and motion after structure.
- Build intents end with a review loop, quick depth unless asked for full.
- Every edit step is preceded by its summary and, where marked, confirmation.
- Platform file always loaded; it changes thresholds, not principles.
