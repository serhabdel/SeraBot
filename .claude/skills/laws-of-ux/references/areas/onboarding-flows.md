# Onboarding, help & multi-step flows

- Value before effort: users can do something useful right away; no unskippable tutorial walls. — Paradox of the Active User
- Help in context (tooltips, inline hints, empty-state guidance), introduced as features become relevant. — Paradox of the Active User, Hick
- Sensible defaults so nobody configures before starting. — Tesler
- Defer non-essential signup fields; explain why sensitive data is requested. — Tesler, Cognitive Load
- Show position and remaining steps ("Step 2 of 4", progress bar). — Goal-Gradient
- Start with credit: first step already checked (Nunes & Drèze 2006: a 10-stamp card with 2 prefilled beat an empty 8-stamp card). — Goal-Gradient
- Incomplete work visible and resumable (setup checklist, drafts, "70% complete"). — Zeigarnik
- Each step: one clear goal, immediate feedback, no interruptions (modals, upsells). — Flow
- Finish strong: a completion moment and a clear first action. — Peak-End

**Rust native pointers:** first-run flags live in config or `use_state` seeded from disk; progress is a `ProgressBar` or step row; a skippable tutorial is a `Link` or button to the main route on every slide, reachable by keyboard.
