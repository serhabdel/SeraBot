# Layout, hierarchy & content

- Space *within* a group visibly smaller than *between* groups; a label sits closer to its own input than to the previous one. Violation: the same gap utility everywhere (`space-y-4` on all children). — Proximity
- Use a shared container (card, background, border) when proximity alone is ambiguous. — Common Region
- Same function → same look (all links alike, all destructive actions alike); different behavior → different look. Static text must not look clickable, and vice versa. — Similarity, Mental Model
- Steps, timelines, segmented controls visually connected. — Uniform Connectedness
- One focal point per view; one dominant CTA. Not every badge red, not every heading colored. — Von Restorff
- Emphasis and status never by color alone (add icon/text/weight; WCAG 1.4.1). — Von Restorff
- Simple, recognizable icons and shapes. — Prägnanz
- Consistent grid, spacing scale and type scale. — Aesthetic-Usability
- Scannable content: headings, short paragraphs, bullets; key info first or last. — Chunking, Serial Position
- Remove words and elements that don't help the task. — Occam, Cognitive Load
- Dashboards/tables: surface the few metrics that matter, the rest on demand. — Pareto, Cognitive Load

Spacing scale example: 4–8px inside a control group · 16–24px between fields · 32–64px between sections.

**Rust native pointers:** the same-gap-everywhere violation looks like `.spacing(8.)` on every `rect()`, one `item_spacing` for every `ui.vertical`, or `.gap_2()` on every `div()`. Grouping containers are `rect().background(..).corner_radius(..)`, `egui::Frame::group`, `div().bg(..).rounded_md()`.
