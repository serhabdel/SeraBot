# Navigation, choices & pricing

**Navigation / IA**
- Conventions: logo top-left → home; primary nav top/left (desktop) or bottom tabs (mobile); cart/account top-right; standard icons. — Jakob
- Labels in users' words, not internal jargon. — Mental Model
- Most important items first and last in a list. — Serial Position
- Few top-level items; rare destinations under secondary nav. Most-used destinations one click away. — Hick, Pareto
- Current location always visible (active state, breadcrumbs, title). — Working Memory
- Don't place key content where ads usually go (right rail, banner styling). — Selective Attention
- Redesigns that move core functions need transition help. — Jakob

**Choices**
- Minimize simultaneous options; long lists get search/filter/sort. A `<select>` beyond ~15 items should be a searchable combobox. — Hick, Choice Overload
- Highlight a recommended/default option. — Hick, default effect
- Progressive disclosure for advanced options. — Tesler, Cognitive Load
- Comparisons side by side with aligned attributes. — Working Memory
- Don't simplify into abstraction (unlabeled icons, hidden essentials). — Hick caveat, Occam

**Pricing / plan pages**
- 3–4 self-serve plans in the main row; "custom/contact sales" tiers separated (strip, row or link), not an equal card. — Hick, Choice Overload
- One plan visibly recommended (badge + border/elevation + the only filled CTA); badge not color-only. — Von Restorff
- Cards list only differentiators ("Everything in Starter, plus:" with 3–6 items), full detail in a comparison table below. 10+ equal bullets per card is a defect. — Chunking, Cognitive Load
- Price, who-it's-for line, CTA in the same position on every card. — Similarity, Jakob

**Rust native pointers:** navigation is `Route` variants (Freya), a screen `enum` (egui) or root view children (gpui); current location is `ActivableRoute`, the matched variant, or the active pane. "Most used one click away" means the hot routes are top-level variants, not nested behind a `Menu`.
