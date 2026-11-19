# Design review rubric (Rust desktop UI)

Check each dimension. Prefer concrete measurements over impressions. API names per framework are in the `stack-map` skill.

## 1. Visual hierarchy and layout
- One clear focal point; obvious primary action. The eye knows where to go first.
- Scan path follows importance (size, weight, color, position).
- Related items grouped by proximity; unrelated separated. Gap between groups at least 2× gap within.
- Everything aligns to shared edges. No near-but-not-quite alignments from mixed `padding` literals.
- Whitespace used as structure, not leftover. Generous around focal elements.

## 2. Typography
- Limited type scale (about 5 to 7 steps), each step clearly distinct (ratio at least 1.2). Sizes come from one module, not scattered `.font_size(15.)` literals.
- Line height: tight for display (1.0 to 1.2), 1.4 to 1.6 for body.
- Measure 45 to 75 characters for body text; capped with a max width.
- Two typefaces at most, deliberate pairing. Consistent weights.
- Changing numbers (timers, counters, prices) use a monospace or tabular family so layout does not jitter.

## 3. Spacing and rhythm
- One spacing scale (4/8 steps). No arbitrary 13 or 27 values in `.padding`, `.spacing`, `Margin::same`, `.p(px(..))`.
- Vertical rhythm consistent between sections; symmetric padding where expected.
- Density appropriate: tool panels may be dense, touch surfaces roomier.

## 4. Color and contrast
- WCAG AA: body text at least 4.5:1; large text (24px, or 19px bold) and UI glyphs at least 3:1. Compute from theme values for both light and dark variants.
- Never color alone for meaning. Add icon, label or shape.
- Restrained palette; accent used sparingly for emphasis and primary actions.
- Disabled and placeholder states readable without looking active (`disabled_alpha`, `weak_text_alpha`, theme muted tokens).

## 5. Motion
- Purposeful: guides attention or shows continuity. Not decoration that delays.
- UI transitions 150 to 250ms; larger entrances 300 to 500ms. Read the actual `.time(..)`, `animate_bool_with_time(.., secs)`, `Duration::from_millis(..)`.
- Ease-out for enters, ease-in for exits; avoid linear except for continuous motion (`Ease::Out` + `Function::Expo`, `ease_out_quint()`).
- Respects the project's reduce-motion setting if one exists; if none exists note it once.
- No layout jumps, no per-frame relayout of large containers, no animation blocking input.

## 6. Component states
- Every interactive element has hover, focus, pressed and disabled states (status state in Freya, `WidgetVisuals` in egui, `.hover/.active/.focus_visible` in gpui).
- Visible focus indicator never removed without replacement.
- Loading, empty and error states designed, not only the happy path (`FutureState`, `QueryStateData`, `Result` branches).
- Hit targets at least 24×24, ideally 40×40 on desktop and 44×44 for touch. Adequate spacing between them.
- Primary versus secondary buttons clearly differentiated.

## 7. Accessibility
- Built-in components over hand-rolled clickable rects. Roles and names where custom: `a11y_role` + `a11y_alt`, `widget_info`, `labelled_by`.
- Icon-only buttons named. Decorative images not focusable.
- Inputs have visible labels; errors adjacent to the field and announced.
- Full keyboard operability: Tab order, arrows inside composite widgets, Escape closes overlays, Enter and Space activate.
- Contrast per section 4; text scales when the user raises the UI scale factor.

## 8. Window sizes
- Holds at the minimum window size the app allows and at a large monitor. Nothing clipped, overlapped or pushed off-screen.
- Text containers wrap or truncate with the full value reachable; no fixed widths on text.
- Panels collapse late and from content, not from device presets.

## 9. Content and copy
- Headlines clear and specific; scannable; no jargon.
- Button labels name the action ("Start scan", not "OK").
- Microcopy guides: placeholders, helper text, human error messages with a recovery path.
- No misleading states (a "Saved" that did not persist).
- Strings templated with pluralization, not concatenated fragments.

## 10. Consistency and theme
- Theme tokens reused for color, type, spacing, radius and shadow. No one-off literals beside an existing token.
- Consistent corner radii (concentric when nested), shadow elevation, icon set and stroke weight.
- Matches the product's voice and visual language across screens.
