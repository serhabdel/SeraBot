---
name: definitive-ux
description: 'The definitive UI/UX orchestrator for any interface in any stack and form factor. ALWAYS use this skill first for anything about an interface, screen, app, website, landing, pricing page, dashboard, form, onboarding, component, layout, theme, tokens, design system, icons, typography, spacing, animation, usability, accessibility, user flow, native feel, or "make it look less generic": Rust desktop (Freya, egui, gpui, Slint, Iced), Rust web (Leptos, Dioxus, Yew), Rust mobile, TV, watch, embedded and HMI, terminal UIs (ratatui), hybrids (Tauri, Electron), plain web (HTML, React, Vue, Svelte), SwiftUI, Compose, Flutter and React Native. Covers build, redesign, review, audit, critique, polish, bolder, quieter, distill, harden, motion, spacing, component cleanup, design source and tokens, art direction, stress tests, PRDs and user stories from code, and quick UX questions, even when the user never says "UX" or "design". It classifies the intent, detects the stack and form factor, picks the specialist skills, runs them in order and delivers one consolidated report.'
---

# definitive-ux

You are the router. You never review, build or audit by yourself. You resolve what the user wants, which stack and which form factor they are on, then run the specialist skills in the order the playbook gives and merge their output into one report. Every specialist owns its rules; this file owns only who runs, when, and how results combine.

Native first, generic by design: the principles are the same everywhere; what changes per stack is the API map, and what changes per form factor is the convention file. Nothing here hardcodes a framework.

## Step 1. Classify the intent

Pick one primary intent. Secondary intents become support steps.

| Intent | Signals (ES/EN) | Primary skill |
| --- | --- | --- |
| **Question** | one specific doubt, a threshold, "which principle applies" | `laws-of-ux` quick mode |
| **Build new** | "quiero hacer", "crea", "arma", "build", new screen/app/site/landing/pricing/onboarding | `design-source` if no theme exists → **persuade**: `art-direction` gate (direction.md before any markup; the laws are constraints inside it) · **operate/read**: `laws-of-ux` build rules + platform file → build → review loop |
| **Redesign** | "rediseñar", "modernizar", "se ve viejo", "hazlo premium", "menos genérico" | `analyze-components` → `design-review` baseline → plan → build → review loop |
| **Review / audit** | "revisa", "critica", "auditar", "antes de lanzar", "is it ready" | `better-interface` full, or `design-review` fast; see Step 4 |
| **Change review** | branch, PR, diff, "lo que cambié" | `better-interface` change-scoped |
| **Polish / feel** | "pulir", "detalles", "no se siente nativo", "craft", "fluido" | `design-review` + `find-animation-opportunities` + platform conventions |
| **Bolder** | "más audaz", "más personalidad", "muy soso", "aburrido" | `art-direction` direction pass + `design-review` |
| **Quieter** | "bájale", "demasiado", "cansa", "sobrecargado" | `design-review` with `slop-tells.md` + `spacing-audit` |
| **Distill** | "simplificar", "quitar", "demasiadas opciones", "menos" | `laws-of-ux` (Hick, Occam, Tesler) + `map-user-flows` to find what carries the task |
| **Harden** | "producción", "edge cases", "errores", "i18n", "qué pasa si falla" | `break-ui` + `better-interface` writing and layout domains |
| **Motion** | "animar", "más vivo", "se siente muerto", "transición", "spring" | `find-animation-opportunities` (where) → `animate` (build one) / `improve-animations` (fix existing) |
| **Spacing** | "espaciado", "desalineado", "padding", "4/8", "ritmo" | `spacing-audit` |
| **Components** | "limpiar componentes", "duplicados", "inventario", "design system" | `analyze-components` |
| **Design source / tokens / theme** | "tokens", "tema", "dark mode", "DESIGN.md", "consistencia entre plataformas", "misma fuente de diseño" | `design-source` |
| **Flows / PRD** | "user stories", "flujo", "mapa de pantallas", "PRD", "qué pantallas faltan" | `map-user-flows` |
| **Stress test** | "qué pasa con nombres largos", "datos reales", "edge cases visuales" | `break-ui` |
| **Ethics** | "dark pattern", "consent", "cancel flow", "pre-checked" | `laws-of-ux` ethics inside whatever review runs |

Ties: prefer the bigger deliverable (build over question, review over spacing) and run the other as support.

## Step 2. Detect stack and form factor

Load `stack-map`. It resolves the framework from the project, loads one verified API map (two for hybrids) and names the form factor. Then load the matching convention file from `references/platforms/`: `desktop`, `mobile`, `tv`, `watch`, `embedded`, `web`, `tui`. State both in the report. Hybrids (Tauri, Dioxus desktop, Electron): content is web, shell is the host platform; findings say which side they belong to.

With only a screenshot or a description: no map; code-level domains are Not verified; the form factor comes from the user.

## Step 3. Register

Every surface is one of three. It changes which rules lead:

| Register | Surfaces | Leads |
| --- | --- | --- |
| **persuade, product** | SaaS landing, pricing, signup, onboarding, feature pages, docs home | `art-direction` in **restraint** mode: convention plus two or three decided details, no concept costume; `laws-of-ux` rules are the floor |
| **persuade, brand** | portfolio, campaign, restaurant, event, launch film, editorial, store page where the design is the message | `art-direction` expressive mode: inventory-driven idea, imagery, one signature move |
| **operate** | tools, dashboards, settings, editors, HMIs, inboxes, shells | `better-interface`; density, keyboard, states; quiet rules from `slop-tells.md` |
| **read** | docs, articles, changelogs, help | calm reading column, identity in the frame |

## Step 4. Review depth

| Situation | Run |
| --- | --- |
| Quick opinion, one screen | `design-review` only |
| Pre-ship, whole app or flow | `better-interface` full with `laws-of-ux` areas for the Why cells |
| "Nothing is broken but something feels off" | `better-interface` full plus ethics and feedback-states areas |
| Dense tool, editor, shell, launcher | `better-interface` with density allowance; motion in reject-heavy mode |
| TV, watch, embedded | `better-interface` with that platform file leading; accessibility domain reads focus and input model first |
| Marketing, landing, pricing, onboarding | `design-review` + `art-direction` critique passes |

Never run `better-interface` and `design-review` on the same scope in one turn.

## Step 5. Run the playbook

`references/playbooks.md` has the sequence for every use case, including which steps are read-only and where the user confirms before edits. Read the matching playbook before invoking anything.

## Step 6. Merge into one report

Unified severity:

| Unified | better-interface | design-review | laws-of-ux | analyze / flows / break-ui |
| --- | --- | --- | --- | --- |
| **Blocking** | HIGH | 🔴 | Defect High | P1 |
| **Important** | MEDIUM | 🟠 | Defect Medium, Risk | P2 |
| **Polish** | LOW | 🟡 | Defect Low, Suggestion | P3 |

Merge rules:

- One root cause is one finding with every location. The owner of the rule keeps the row; others fold their Why into it.
- Every finding keeps `file:line` or the exact screen and element. No evidence, no finding.
- Purpose test on every visual finding and every build decision (`references/slop-tells.md`): a technique with no stated purpose is a finding; removing it without adding substance is also a finding. On a persuade build, a missing `design/direction.md`, an unstated default, a category palette, a performance tell on a product surface, or an invented product feature is Blocking on the build itself; the category-reflex check and the AI-feel pass run before delivery.
- At most one or two Laws of UX per finding.
- Cap 15 (full) or 5 (quick); Blocking never falls off the cap.
- Read-only by default. Edits only when asked or where a playbook step says "after confirmation". Rust edits follow `rust-engineering` and `cargo check`.
- Optional numbered mode: when the user wants to pick, list findings numbered, wait, fix only the numbers chosen, report.
- End with the single highest-leverage change and a verdict: `Block`, `Approve`, or `Ship with polish list`.

### Rule ownership

| Topic | Owner |
| --- | --- |
| Keyboard, focus, names, hit areas, reduced motion | `better-interface` accessibility (platform file sets the numbers) |
| Grouping, alignment, hierarchy, density | `better-interface` layout |
| Copy, errors, empty states | `better-interface` writing |
| Type scale, measure, truncation | `better-interface` typography |
| Contrast numbers | `design-review` (contrast script) |
| Whether motion belongs | `find-animation-opportunities`; rules in `motion-standards.md` |
| Building one animation | `animate` |
| Fixing existing motion | `improve-animations` |
| Spacing values vs scale | `spacing-audit` |
| Duplicates, orphans, naming, hardcoded tokens | `analyze-components` |
| Tokens, theme, DESIGN.md | `design-source` |
| Direction, imagery, tells on persuade surfaces | `art-direction` |
| Missing screens and states across a flow | `map-user-flows` |
| Worst-case data | `break-ui` |
| Dark patterns, defaults, choice counts, thresholds | `laws-of-ux` |
| API names per framework | `stack-map` |
| Platform conventions | `references/platforms/*` |

## Report skeleton

```markdown
# UX report: <scope>
**Intent:** … · **Stack:** … · **Form factor:** … · **Register:** … · **Depth:** … · **Skills run:** …
**Assumed primary task and user:** …
**Verdict:** Block | Approve | Ship with polish list — one sentence why.

## Blocking
### 1. <title>
- Evidence: `file:line` or screen/element · Why: <rule, law if useful> · Fix: <exact value in the project's idiom>
## Important
## Polish
## Risks (need data to confirm)
## Suggestions (product decisions, stated as assumptions)
## Keep
## Verification run
## Next step
```

Empty sections are omitted. Build intents replace the report with the artifact plus a UX decisions note (3–6 bullets, decision + law), then the review loop runs on what was built.

## Manual invocation

Bare skill names: `definitive-ux`, `stack-map`, `laws-of-ux`, `design-review`, `better-interface`, `find-animation-opportunities`, `animate`, `improve-animations`, `spacing-audit`, `analyze-components`, `design-source`, `art-direction`, `map-user-flows`, `break-ui`. Claude Code namespaces them under the plugin (`/definitive-ux:design-review`); other hosts use the bare name. A direct call skips the router but keeps the severity scale and the skeleton.

Triggering: Claude Code gets a `UserPromptSubmit` hook from the plugin; other hosts rely on descriptions plus `templates/AGENTS.snippet.md`.

## Other tools

- `rust-engineering` (if installed) for any Rust edit; `freya` skill for Freya idioms beyond the map.
- `codebase-memory-mcp` for reach and callers when available; otherwise the host's symbol search.
- The `run` skill, `freya-testing`, `egui_kittest`, `slint-viewer`, browser tooling: whenever a visual claim needs a render.

Answer in the user's language. Law, skill and API names stay in English.
