---
name: art-direction
description: 'Art direction for surfaces where the design is the product: landing pages, marketing sites, portfolios, launch screens, onboarding, product pages, campaign pages, in any stack (web, Rust web with Leptos or Dioxus, or a native app''s showcase screens). Replaces the generic default (centered headline, three icon cards, gradient) with decisions: an inventory of the subject, an argument per section, a composed direction, a type and color system from the subject, real imagery, one signature motion, then a look-and-fix loop on the render. Use when the user asks to build or redesign a site, landing, hero, portfolio or marketing page, or says a page looks generic, templated or AI-made. Usually invoked by the definitive-ux router; can be called directly.'
---

# Art direction

> **Stack:** load `stack-map`. Tells and the purpose test come from `definitive-ux/references/slop-tells.md`; tokens go through `design-source`. Method adapted in prose from taste-skill (its study of 247 gallery sites) and impeccable's brand register; no scripts, no asset library.

A page built by a model is the average of every page it has seen: nothing wrong, nothing decided. This skill makes decisions and writes them down before any markup.

## When it applies

Persuade register only: the visitor came to be convinced. Operate and read surfaces (dashboards, settings, docs) follow `better-interface` and `design-review` with the quiet rules in `slop-tells.md`; a product site that shows its app is persuade, and the app fragments on it follow the app-screen rules for their insides.

## The gate (non-negotiable)

No markup, no component, no stylesheet is written until `design/direction.md` exists and contains, in this order: the inventory (8 to 12 items), the argument (one claim and proof per section), three candidate directions in two lines each, the chosen one with the sentence that says why only this subject could own it, and the direction sheet (type system, palette as proportions with values from the subject, hero composition, a blueprint per section, imagery plan, the one signature motion). When the router or a playbook hands you a build, the first file you write is that one. A build that starts from "the rules say four cards and a strip" has skipped this skill; the rules are the floor, not the direction.

**Decided means stated, not different.** Every one of these cites its reason in the sheet: typeface, palette, hero composition, imagery, the signature motion. A reason can be restraint: "system font stack, because the page should feel like the product UI it sells" is a decision; "Bricolage Grotesque, because it looks designed" is a costume. What this skill prevents is the *unstated* default and the *performed* decision alike. Run the category-reflex check at two altitudes (`slop-tells.md`): first order, could someone guess palette and type from the category alone; second order, could they guess the aesthetic family from the category plus the obvious anti-reflex (fintech that is not navy becomes warm paper and crimson, or terminal dark). Both are reflexes.

**Restraint is a direction, and the default one for product surfaces.** This is the sources' verdict, not a preference: the dated tells registry calls the warm-editorial concept look the newest generated style; the second-order reflex check catches the anti-reflex; Jakob's law names nav, forms, checkout and pricing as the places where convention wins; the purpose test rejects technique without purpose; the app-screen measurements put color on data, not chrome. A SaaS pricing page, a signup, a B2B landing, a docs home: the visitor wants to understand and decide, not to be impressed. There the right direction is convention (Jakob) plus two or three decided details: one quiet typeface pairing or the product's own, one accent carrying the single action, generous spacing, real content, a real product fragment as the only image. Expressive directions (a full-bleed photograph, a typographic poster, a catalogue, a timetable, a letter) belong to brand surfaces: portfolios, campaigns, restaurants, events, launches, where the design is the message. When the brief is a product, choose restraint unless the inventory gives a reason not to, and write that reason down.

**Performance tells (banned on product surfaces, earned elsewhere only by the inventory).** These are how a model performs "having an idea": a page styled as the object it sells (invoice, ticket, receipt, passport); rubber stamps, rotated badges, tilted anything; sentence-fragment headlines with a full stop ("No small print."); one word of the headline in the accent color; em dashes or arrows as bullets; three typefaces; a characterful display grotesk at 0.9–1.0 leading over a quiet sans; warm off-white ground with a crimson or terracotta accent; a mono face for anything but code and numbers; a clever metaphor in every section heading; an interactive gadget the product does not have (an estimator, a calculator, a configurator) invented to make the page feel alive. A page that avoids the first-wave tells and lands on these has not escaped the generator; it has found its newest costume.

**Never invent product features.** Prices, limits, FAQ answers and copy may be plausible placeholders marked as such. New product capabilities (a usage estimator, a live demo, a chatbot) are product decisions; propose them in the notes, never build them unasked.

**Laws are constraints, not the design.** Pricing, onboarding and signup pages come with rules from `laws-of-ux` (one recommended plan, differentiators only, enterprise separated, verb-first buttons). Keep every one of them and still run the gate: the rules say what must be true, the direction says what it looks like.

## The run

### 1. Read the brief and inventory the subject

What is real: the subject, the audience, the one thing the page must get someone to do, the stack, the brand that exists (colors, fonts, logo), and above all what imagery truly exists (own photographs, product shots, a real interface, an image tool, or nothing). Never plan around pictures you do not have.

Then list eight to twelve things that belong to this subject and to no competitor: what the name means; the street, city, materials and light of its world; the numbers in the brief (seats, minutes, price, year); the constraint it lives with; its rituals and hours; the printed things of its world (a ticket, a label, a timetable). The idea comes from this list. If the request is vague, decide and state your reading in one line; ask only when a missing fact blocks the work.

### 2. Write the argument

One line per section: the claim and its proof. The claims decide which sections exist and in what order. Delete any section with no claim. This is how the stock sequence (hero, logos, three features, testimonial, pricing, CTA) never appears by default.

### 3. Choose a direction

Write three candidate directions in two lines each: the idea taken from the inventory, the composition that carries it (a full-bleed photograph with the title set into it; a typographic page with one enormous word; a catalogue of real objects; a timetable; a letter), the type class (grotesk, humanist, serif, slab, display + text), the palette as proportions (one ground, one ink, one accent, fields) with colors taken from the subject, and the signature motion (one). Choose the one that only this subject could own; write one line on why. Then fix the direction sheet: type scale, palette values, hero composition, a blueprint per section of the argument, imagery plan.

Adopt it whole. Redraw one slot only for a reason you can state (a brand constraint, an asset that does not exist). Never redraw toward the familiar.

### 4. Imagery

Real pictures of this subject's world: the project's own, or open archives and museums credited, or a real interface fragment built in the stack with real data. A product with no photographs is drawn or set among real pictures of its world, never a fake product shot. At least four real pictures on a page; crops share ratios; every image does a job (shows the thing, sets the place, proves the claim). Grey boxes, stretched photos and fake dashboards made of bars are the weakest pictures a page can have.

### 5. Build

Tokens through `design-source` (the direction's palette and type become the semantic layer). Semantic structure, visible focus, labels, reduced motion, responsive from 320px. The hero carries the idea; every later section carries its claim with its full content. Type set large; whole fields of color; sections separated by space and tone, not hairlines. One authored motion moment, exponential ease-out, nothing on every section.

### 6. Look, then fix

Render at three widths and judge the screenshots, never the code. Use whatever can render: the `run` skill, a browser automation tool, a headless browser, a screenshot the user takes. **With no renderer available, the loop still runs in degraded form**: open the HTML and read it as a stranger would, top to bottom, answering every pass below from the markup and the computed sizes you can infer; say in the report that the critique was done without a render and name the three things a render would settle (actual measure, image crops, motion). Skipping the passes because there is no browser is not allowed.
Render at three widths (mobile, tablet, desktop) and judge the screenshots, never the code. Fourteen passes, each on its own:

1. One second, then five: what did you see? There must be one answer. Can a stranger say what this is, who it is for and what to do next?
2. Squint: two or three large masses with clear size differences, or many medium ones?
3. Cover the logo: could this belong to a competitor? Does the idea reach the pricing, the FAQ and the footer?
4. Edges: count distinct left edges of text blocks; two or three reused on purpose, not six.
5. Space and frame: related close, unrelated far; gaps between sections larger than any inside; no accidental holes; left and right margins equal; no section leaving half its width empty; no stretch of nothing taller than half a screen.
6. Type: body-to-headline jump decisive (3× or more); five sizes at most; no orphan word; no paragraph over 75 characters or centered over three lines.
7. Color: which holds most of the surface, which supports, where is the accent; the accent appears on the single action and one detail.
8. Images: at least four real ones, every one doing a job, crops sharing ratios, nothing grey or stretched.
9. Tells: run `slop-tells.md` on the render.
10. Controls: every button named for its action; one dominant action per view.
11. States: hover, focus, loading, error, empty exist.
12. Mobile: the same idea survives at 320px; nothing stacks into a generic column.
13. Motion: one moment, earned, reduced-motion branch present.
14. Substance: did removing the tells leave a hole? Add type size, a field of color, a real picture, full content.

Fix the worst finding, render again, stop when the one-second pass gives one answer and the tells pass is clean.

## Calibration

Two builds of the same brief, a B2B electronic-invoicing SaaS pricing page. The one that read as crafted: conventional four-card row with Enterprise as a strip, one quiet typeface, navy ink and one green used only on the recommended plan's button and checks, white ground, generous spacing, differentiators on cards and the full matrix below, FAQ in `details`, no imagery. Decided through restraint and content. The one that read as generated: warm paper ground, crimson accent, three typefaces with a characterful display grotesk at 0.95 leading, a rotated rubber stamp as the recommended badge, em-dash bullets, fragment headlines with full stops, a usage-estimator slider the product never had. It followed the gate to the letter and performed every decision. Restraint plus two or three decided details beats a concept every time on a product surface; keep the concept for the surfaces whose job is to be remembered.

## Output

`design/direction.md` (written first, kept), the built page, the credits for any found imagery, the degraded-or-rendered critique with what was fixed, and a UX decisions note (3–6 bullets, each decision naming its inventory source and, where one applies, its law). Then the router's review loop runs `design-review` on the result.
