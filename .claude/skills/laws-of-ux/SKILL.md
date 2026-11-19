---
name: laws-of-ux
description: 'Apply the 30 Laws of UX (lawsofux.com: Fitts, Hick, Jakob, Miller, Doherty, Tesler, Postel, Peak-End, Gestalt proximity/similarity/common region, Von Restorff, Zeigarnik, Goal-Gradient, cognitive load, choice overload and the rest) as concrete decisions about an interface in any stack: Rust desktop (Freya, egui, gpui), web or mobile. Use for quick questions ("how big should this button be", "how many options", "which principle applies", "is 600ms ok"), as the Why layer inside any UI audit, and as the decision rules before building a screen, form, onboarding, pricing page, navigation or dashboard. Also the owner of dark-pattern and consent checks. Usually invoked by the definitive-ux router; call directly for quick answers.'
---

# Laws of UX

Turn the Laws of UX into concrete decisions about *this* interface: an observable problem, the one principle that explains it, a specific fix in the project's stack. Principles catalogued by Jon Yablonski at lawsofux.com; summaries here are our own words, each law links to its page.

## Pick the mode

| Mode | When | Read |
| --- | --- | --- |
| **Quick answer** | One question: a law, a threshold, "which principle applies" | Only this file |
| **Why layer** | Another skill (`better-interface`, `design-review`) is auditing and needs the law behind a finding | This file + at most 3 area files |
| **Audit** | Standalone review of code, screenshot, description or flow when no other review skill runs | This file + at most 3 area files |
| **Build** | Decide rules before creating a screen, component or flow | This file + at most 3 area files |

Area files in `references/areas/`: `layout-content.md` (hierarchy, grouping, emphasis) · `navigation-choices.md` (nav, option counts, pricing) · `forms-input.md` (forms, validation, selects, targets, code red flags per stack) · `feedback-states.md` (latency, loading, empty, error, success) · `onboarding-flows.md` (onboarding, multi-step, progress) · `ethics.md` (defaults, dark patterns). `references/laws.md` has every law condensed with its source link, for theory or citation requests only.

**Stack idioms** come from elsewhere: Rust native from the `stack-map` skill, web and mobile from `stack-map/maps/web.md`. This skill names the rule and the threshold; the map names the API.

**Quick answers are short:** under 200 words, no tables, at most one small snippet, one law. Answer, reason, fix. Longer only on request.

## The 30 laws

**Perception:** Proximity (near = grouped) · Similarity (alike = related) · Common Region (shared container groups) · Uniform Connectedness (connected = related) · Prägnanz (simplest reading wins) · Von Restorff (the different one is noticed) · Aesthetic-Usability (pretty feels usable and can mask problems)
**Attention and memory:** Selective Attention (ad-like = ignored) · Serial Position (first and last remembered) · Miller (about 4 chunks held in mind) · Working Memory (do not make users carry info) · Chunking · Zeigarnik (unfinished sticks) · Peak-End (worst or best moment plus the ending)
**Decisions:** Hick (more options, slower) · Choice Overload · Cognitive Load · Cognitive Bias (defaults, anchoring; use ethically) · Tesler (the system absorbs complexity) · Occam (fewest parts) · Pareto (20% of features carry 80% of use)
**Expectations:** Jakob (work like familiar products) · Mental Model · Paradox of the Active User (nobody reads manuals) · Postel (liberal input, strict output) · Parkinson (shorten tasks)
**Time and motivation:** Doherty (under 400 ms) · Fitts (big, close targets) · Goal-Gradient (show progress) · Flow (clear goals, instant feedback)

## Thresholds

- **Targets:** iOS 44×44 pt · Android 48×48 dp · web and desktop minimum 24×24 px (WCAG 2.2 SC 2.5.8 AA), 44×44 recommended (2.5.5 AAA), 40×40 fine on dense desktop tools
- **Response:** 100 ms or less feels instant · under 400 ms keeps engagement (Doherty) · over 1 s needs an indicator and a disabled re-submit · over 10 s needs determinate progress and cancel
- **Memory:** about 4±1 chunks; Miller's 7±2 is about what users must hold in mind, not menu length
- **Choices:** a select beyond about 15 options needs search; 3 to 4 plans on a pricing row; one dominant action per view
- **Text:** 45 to 75 characters per line · contrast 4.5:1 body, 3:1 large text and UI glyphs

## Audit rules

1. State the assumed primary task and user in one line.
2. Every finding points to observable evidence: `file:line`, element, value, count.
3. Classify: **Defect** (demonstrable) · **Risk** (depends on data you lack; say what confirms it) · **Suggestion** (product-context ideas such as wallets, geo-IP, sticky CTAs; never presented as defects; state the assumption).
4. Cite at most one or two laws per finding, only when they explain the harm.
5. Fewer, stronger findings: typically 4 to 8 defects and risks. Merge related ones.
6. Fixes in the project's stack and idiom, minimal snippets.
7. When running inside `definitive-ux`, hand findings to the unified report instead of this format.

```markdown
# UX review: <screen>
**Assumed primary task:** … · **Verdict:** <2 sentences>
## Defects
### 1. <title> — High | Medium | Low
- **Evidence:** … · **Impact:** … · **Why:** <law, if useful> · **Fix:** …
## Risks
- **<title>** — <evidence> → <impact>. Confirm with: … Fix: …
## Suggestions (context-dependent)
- <idea> — *if* <assumption>
## Keep
- <what works>
```

High = blocks or slows the primary task or loses data · Medium = noticeable friction · Low = polish.

## Build rules

Apply the laws silently. Decide before coding: one primary action per view · spacing tighter inside groups than between · conventional patterns for core interactions · fewer simultaneous choices (recommend a default, list only differentiators, move edge-case tiers aside) · loading, empty, error and success states · adequate targets and forgiving inputs · progress for multi-step flows · head start on the first step. Do not invent product decisions (billing toggles, discounts, policies); mark placeholder data as such. End with a **UX decisions** note: 3 to 6 bullets, "decision, law".

## Conflicts

Name the tradeoff: Hick and Occam vs Tesler (remove options only if the system can decide; else progressive disclosure) · Jakob vs differentiation (convention for nav, forms, checkout; personality elsewhere) · Aesthetic-Usability (impressions are not task success; a beautiful screen still gets the keyboard test) · Von Restorff (emphasize one thing, or nothing is emphasized) · Cognitive Bias (flag manipulative defaults, see `areas/ethics.md`) · Doherty vs honesty (a deliberate short delay can build trust for heavy operations; never fake progress).

Answer in the user's language; law names stay in English.
