# Slop tells, purpose test and liveliness

Three ideas merged: a dated registry of tells (from taste-skill), a refuse list (from impeccable's craft floor) and the purpose test with the liveliness bar (from Anti-Slop). Our own words. Used by `design-review`, `better-interface`, `art-direction` and the build playbooks.

## The purpose test

Before using any visual technique ask: what does this serve? Hierarchy, identity or readability are answers; "it looks modern", "it looks safe", "landing pages do this" are not. A technique with no answer is removed or rewritten, and the answer for the ones kept is written down in the UX decisions note.

**The swap test.** Swap the logo and the product name. If the design could belong to a competitor or another industry, it has no idea. Go back to the direction.

**Removal is not design.** Taking the tells away leaves a void; a page stripped of ornaments and given nothing in their place reads as thin and loses to a richer page with tells. The other half is substance: larger type, whole fields of color, real pictures of this subject's world, every section carrying its full content, one authored moment of motion. Liveliness is added, never assumed.

## Dated registry

Tells are devices produced by default, everywhere, without a reason. They were good ideas once. Replacements are procedures (derive from the subject, draw from the direction), never a product, because every "use this instead" list becomes the next wave.

### First wave (2023–2024)

| Tell | Status | Instead |
| --- | --- | --- |
| Violet or indigo gradients on hero, buttons, glows | banned | flat color from the direction |
| Gradient-filled headline text | banned | solid ink; emphasis by weight or size |
| Glass panels, backdrop blur as decoration | warned (2+) | opaque surfaces one tone step apart |
| Glowing or gradient primary button | banned | flat button named for its action |
| Soft grey drop shadow under every card | banned | tone step, or no box |
| Three identical feature cards with an icon in a rounded square | banned | a composed section from the argument |
| Emoji as icons or bullets | banned | words, drawn icons, real pictures |
| Centred headline, subtitle, two buttons, screenshot below | avoided by the draw | the dealt hero |
| One default typeface on every project | warned | the direction's type system |

### Second wave (2025)

| Tell | Status | Instead |
| --- | --- | --- |
| Eyebrow or kicker: tiny tracked uppercase above every heading | banned | fold into the heading or delete |
| Pill badge above the hero headline ("New", "Beta") | banned | say it in the supporting sentence |
| One italic serif word in the headline | warned | one face, one color |
| Bento grid of rounded tiles as the feature section | warned | a composed section; tiles only when the direction is dense tiles |
| Near-black ground, one neon accent, a glow | banned | a dealt dark palette, no glow |
| Floating rounded pill navigation with blur | warned | a plain bar |
| Fake dashboard hero of grey bars and donut charts | avoid | a real interface fragment or no image |
| Pulsing status dot ("Available for work") | lint | remove |
| Fade-and-rise on every section | avoid | one signature move |
| Numbered section markers 01 / 02 / 03 by reflex | banned | numbers only when the sequence carries information |
| Hero metric template: big number, small label, supporting stats | banned | content |
| Side-stripe `border-left` accent on cards and alerts | banned | full border, tint, leading icon, or nothing |

### Third wave (2025–2026): the fix that became the look

| Tell | Status | Instead |
| --- | --- | --- |
| Cream or sand ground with terracotta, rust or clay accent | banned together; terracotta banned alone; cream warned | warmth from accent, type, imagery or a full warm field, not a near-white tint |
| Token names `--paper`, `--cream`, `--sand`, `--bone`, `--linen`, `--parchment` | tell in themselves | name by role |
| High-contrast serif display as the shortcut to "premium" | no rule | the direction's type, whatever class |
| Broadsheet cosplay: hairlines everywhere, zero radius, dense columns | banned via hairline | structure from space and tone |
| Performative labels: "Field notes", "Quietly trusted by" | lint | plain section names |
| Monospace as a costume for "technical" | refuse | monospace for code, data, measurement |
| Hard offset block shadow outside a real neobrutalist world | refuse | a depth system the world chose |

### Gallery fashion, refused as house style

Monospace labels, index numbering, hairline rules, uppercase micro-labels: common on award sites and the first things a generator copies. Carry no information. A page built without them has to find real structure.

### Structural tells

Same-size card grids as page structure; nested cards (always wrong); a modal for a task that needs no interruption; a kicker above a heading; six distinct left edges on one page; a section with half its width empty; a stretch of nothing taller than half a screen; the same gap utility on every child.

### Copy tells

"Oops!", exclamation marks, "Seamless", "Effortless", "Elevate", "Unlock", "Supercharge", "Welcome back 👋", blame in errors, "Learn more" repeated, buttons that say "Submit" or "OK".

### Motion tells

Fade-up on every section; bounce on functional UI; entrance animations on first render of existing state; parallax by default; a spinner where a skeleton or optimistic update fits; animation on keyboard-driven surfaces.

### Native app tells

Web-shaped controls in a native app (hover-dependent affordances, HTML-style buttons, custom toggles); reinvented navigation bars; a custom titlebar that breaks window dragging; no native menu; mixed icon sets; a desktop app that is a phone layout stretched; a TV app with hover states; a watch app with a form; an embedded HMI with thin weights and gradients.

### Dashboard tells

Welcome banner with a waving hand; four KPI tiles each with an icon in a tinted circle, a green percentage and a sparkline; gradient area chart; zebra tables under uppercase headers; coloured sidebar pill. Real product screens: about 3% coloured pixels, KPIs as type not tiles, flat surfaces separated by a 1px line or a tone step, no hero header, charts with almost no chrome, navigation that recedes, zeros printed as zeros.

## How to use

In review: a tell is a finding only when it fails the purpose test for *this* surface; cite the row. In build: run the swap test on the first viewport before writing more; run the registry on the result; then add liveliness where the removal left a hole.

## Absence of decision (not on any tell list, and the most common failure)

A page can avoid every tell above and still read as generated, because nothing in it was chosen. The signs: the system font stack or the one typeface every generated page uses; a palette anyone could guess from the category; the stock section sequence; no picture of the subject's world; every size and gap from the framework defaults. These are not bans; they are the absence of a decision, and the fix is the `art-direction` gate: inventory, argument, direction sheet, each choice citing its source.

**Category-reflex check, two altitudes.** First order: could someone guess the theme, palette and type from the category alone (finance → navy and green; artisanal → cream and terracotta; developer tool → near-black and neon; wellness → pink and sage; kids → primary colors and rounded everything)? Second order: could they guess the aesthetic family from the category plus the obvious anti-reflex (fintech that is not navy → terminal dark mode; artisanal that is not cream → broadsheet serif)? Rework until neither answer is obvious. On operate surfaces the check still applies to the brand layer (accent, type, icon set), not to the controls.

## Performance tells (the costume of having an idea)

When a generator is told to decide, it performs deciding. The result reads as generated more loudly than the first-wave defaults did, because every move says "look, a choice". Banned on product surfaces (SaaS pages, pricing, signup, docs, tools); earned on brand surfaces only when the inventory names the reason:

| Tell | Instead |
| --- | --- |
| Page styled as the object it sells: invoice, ticket, receipt, boarding pass, passport | the object as one real fragment, the page as a page |
| Rubber stamps, rotated or tilted badges, "hand-placed" elements | a badge in the flow, aligned |
| Sentence-fragment headlines with a full stop ("No small print.") | a headline that says the claim |
| One word of the headline in the accent color | one color for the headline |
| Em dashes, arrows or hand glyphs as list bullets | plain bullets, checks, or no bullets |
| Three typefaces; a characterful display grotesk at 0.9–1.0 leading over a quiet sans; mono for labels | one family, two at most, from the product or the subject |
| Warm off-white ground with crimson, terracotta or ochre accent | the product's palette, or white and one accent |
| A clever metaphor in every section heading | plain section names, the claim in the heading |
| An interactive gadget the product does not have (estimator, calculator, configurator) | the product's real fragment; a gadget only if it exists |
| "Only one animation" that is a staged entrance of a decoration | a transition on a real state change, or none |

The AI-feel pass: before delivery, name the three elements a working designer would point at first and say "a model made this", and remove them. If none can be named, look again.
