<!-- verified: 2026-10-22 against local sources: freya 0.5.0-rc.8 (contributions clone), egui 0.32.1 (contributions clone), gpui 0.2.2 (sonora pinned rev 5ff26c7) -->
---

# Rust desktop core map: Freya, egui, gpui

Shared recon for the UI review skills. Resolve the framework once, then use the matching column everywhere. Never propose a fix written for another framework or for CSS.

## 1. Detect the framework

Read `Cargo.toml` (workspace root and the crate under review):

| Dependency | Framework | Paradigm |
| --- | --- | --- |
| `freya`, `freya-components`, `freya-router` | Freya 0.5 (0.4 projects use the older `rsx!` macro) | Retained, declarative, builder elements, hooks |
| `egui`, `eframe` | egui 0.32 | Immediate mode, `ui: &mut Ui` per frame |
| `gpui` | gpui 0.2 (Zed) | Retained views, Tailwind-like builder, actions + key bindings |

Then run the repository index: `list_projects` and `index_repository` from `codebase-memory-mcp` so callers, orphans and fan-out come from the graph instead of grep.

Read the project's own conventions before judging: `CLAUDE.md`, `AGENTS.md`, `CONTRIBUTING.md`, a `theme.rs` or `tokens.rs`, a `design.md`. A documented convention settles taste. It never excuses an accessibility or keyboard failure.

## 2. Where things live

### Components and screens

| | Freya | egui | gpui |
| --- | --- | --- | --- |
| Stateful component | `#[derive(PartialEq)] struct X; impl Component for X { fn render(&self) -> impl IntoElement }` | `fn ui(&mut self, ui: &mut egui::Ui)`, `impl egui::Widget`, `impl eframe::App` | `impl Render for X { fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) }` |
| Stateless piece | `fn thing(..) -> impl IntoElement` | `fn thing(ui: &mut Ui, ..)` | `#[derive(IntoElement)] struct X; impl RenderOnce` |
| Primitive elements | `rect()`, `label()`, `paragraph()`, `image()`, `svg()` | `ui.label/heading/button/checkbox/...`, `egui::Frame`, panels, `Window`, `Modal` | `div()`, `img()`, `svg()`, text via `.child("..")` |
| Conditional render | `.maybe(cond, \|r\| ..)`, `.map(opt, ..)`, `.maybe_child(opt)` | `if cond { ui.add(..) }` | `.when(cond, \|d\| ..)`, `.when_some(opt, ..)`, `.children(iter)` |
| List keys | `.key(id)` on items in `VirtualScrollView` | `ui.push_id(id, ..)` | `.id(ElementId)`, `uniform_list` |

### Spacing, shape, surfaces

| | Freya | egui | gpui |
| --- | --- | --- | --- |
| Padding | `.padding(8.)`, `.padding((10., 24.))`, `Gaps::new(..)` | `Frame::new().inner_margin(Margin::same(8))` (`i8`), `ui.spacing().button_padding` | `.p_2()`, `.px_3()`, `.py_2()` tokens, arbitrary `.p(px(14.))` |
| Gap between children | `.spacing(8.)` | `ui.spacing_mut().item_spacing = vec2(8., 8.)`, `ui.add_space(8.)` | `.gap_2()`, `.gap(px(..))` |
| Margin | `.margin(..)` | `Frame::outer_margin(..)` | `.m_2()`, `.mx_auto()` |
| Radius | `.corner_radius(8.)`, `CornerRadius::new(..)` | `Frame::corner_radius(CornerRadius::same(8))` (`u8`), `visuals.window_corner_radius` | `.rounded_md()`, `.rounded(px(..))` |
| Shadow | `.shadow((0., 4., 20., 4., (0, 0, 0, 80)))`, `Shadow::new()` | `Frame::shadow(Shadow {..})`, `visuals.window_shadow` | `.shadow_sm()`, `.shadow_md()`, `.shadow_lg()` |
| Border | `.border(Border::new()..)`, `BorderAlignment::Inner` | `Frame::stroke(Stroke::new(1., color))` | `.border_1()`, `.border_color(..)` |
| Size | `.width(Size::fill())`, `Size::px`, `Size::percent`, `Size::flex`, `.expanded()` | `ui.set_width`, `ui.set_min_size`, `allocate_ui` | `.w_full()`, `.size_4()`, `.flex_1()`, `.min_w_0()` |
| Alignment | `.center()`, `.main_align(..)`, `.cross_align(..)`, `.horizontal()`, `.vertical()` | `ui.horizontal`, `ui.vertical`, `Layout`, `ui.columns` | `.flex()`, `.flex_col()`, `.items_center()`, `.justify_between()` |

gpui spacing tokens follow Tailwind rems at 16px root: `_0p5` 2px, `_1` 4, `_1p5` 6, `_2` 8, `_2p5` 10, `_3` 12, `_4` 16, `_5` 20, `_6` 24, `_8` 32, `_10` 40, `_12` 48, `_16` 64, `_20` 80, `_24` 96. Any `.p(px(n))` or `.gap(px(n))` literal is an escape hatch and should be checked against the scale.

### Typography

| | Freya | egui | gpui |
| --- | --- | --- | --- |
| Size | `.font_size(16.)` | `TextStyle::{Heading, Body, Small, Button, Monospace}` via `style.text_styles`, `RichText::size` | `.text_xs()` 12px, `.text_sm()` 14, `.text_base()` 16, `.text_lg()` 18, `.text_xl()` 20, `.text_2xl()`, `.text_3xl()` |
| Weight | `.font_weight(..)` | `RichText::strong()`, `.weak()` | `.font_weight(FontWeight::SEMIBOLD)` |
| Line height | `.line_height(1.4)` | font size only; `Spacing::item_spacing.y` between rows | `.line_height(relative(1.5))` |
| Wrap and truncate | `.max_lines(1)`, `.text_overflow(TextOverflow::Ellipsis)` | `TextWrapMode::{Wrap, Truncate, Extend}` via `style.wrap_mode` or `Label::truncate()` | `.truncate()`, `.text_ellipsis()`, `.line_clamp(n)`, `.whitespace_nowrap()`, `.overflow_hidden()` |
| Alignment | `.text_align(TextAlign::Center)` | `Layout` alignment | `.text_center()` |
| Letter spacing | `.letter_spacing(..)` | none | none |

### Interaction states and feedback

| | Freya | egui | gpui |
| --- | --- | --- | --- |
| Hover | `.on_pointer_enter` / `.on_pointer_leave` writing a `use_state` status, then style from it | `response.hovered()`; `visuals.widgets.hovered` | `.hover(\|s\| s.bg(..))`, `.group_hover(..)` |
| Press | `.on_press`, `.on_pointer_down` / `.on_pointer_up` | `response.is_pointer_button_down_on()`; `visuals.widgets.active` | `.active(\|s\| ..)`, `.on_mouse_down` |
| Click | `.on_press` (keyboard and pointer), `.on_mouse_up` | `response.clicked()` | `.on_click(cx.listener(..))` on an element with `.id(..)` |
| Disabled | prop and style branch | `ui.add_enabled(false, w)`, `ui.disable()`, `visuals.disabled_alpha` | style branch; `.cursor_not_allowed()` |
| Cursor | `.cursor(..)` | `response.on_hover_cursor(CursorIcon::PointingHand)` | `.cursor_pointer()` |
| Tooltip | `Tooltip` component from `freya-components` | `response.on_hover_text("..")`, `on_hover_ui` | `.tooltip(\|window, cx\| ..)`, `.hoverable_tooltip` |
| Loading, empty, error | branch in `render` on `FutureState` or `QueryStateData` | branch in `ui` | branch in `render` |

### Keyboard, focus, accessibility

| | Freya | egui | gpui |
| --- | --- | --- | --- |
| Focusable | `let focus = use_focus();` then `.a11y_id(focus.a11y_id())`, `.a11y_focusable(true)`, `.a11y_auto_focus(true)` | widgets with `Sense::click()` are focusable; `response.request_focus()`, `has_focus()` | `cx.focus_handle()`, `.track_focus(&handle)`, `impl Focusable`, `.tab_index(n)`, `.tab_stop(true)` |
| Visible focus | style branch on `focus.is_focused()` | `visuals.widgets.hovered`/selection stroke; egui draws a focus outline for native widgets | `.focus(\|s\| ..)`, `.focus_visible(\|s\| ..)`, `.in_focus(..)` |
| Key handling | `.on_key_down`, `.on_key_up`, `.on_global_key_down`, `Key::Character`, `Key::Escape` | `ui.input(\|i\| i.key_pressed(Key::Escape))`, `Modifiers`, `FocusDirection::{Next, Previous, Up, Down}` | `actions!(ns, [Close])`, `.key_context("Name")`, `.on_action(cx.listener(..))`, `cx.bind_keys([KeyBinding::new("escape", Close, Some("Name"))])`, `.on_key_down` |
| Accessible name and role | `.a11y_role(AccessibilityRole::Button)`, `.a11y_alt("..")`, `.a11y_member_of(..)`; built on AccessKit | `response.widget_info(\|\| WidgetInfo::labeled(WidgetType::Button, enabled, "Save"))`, `response.labelled_by(label_id)`; needs the `accesskit` feature on egui or eframe | AccessKit wiring in `window/a11y.rs`; limited, so report what the element exposes and mark the rest **Not verified** |
| Modal focus | `Popup` from `freya-components` traps focus; custom overlays must restore focus manually | `egui::Modal`, `Window` with `.open(&mut bool)`; close on Escape via input | overlay view that owns a `FocusHandle`, restores focus on close |
| Reduced motion | no OS query exposed; a project setting or env var is the convention | none built in; `style.animation_time = 0.` disables | none built in; gate `with_animation` behind a project setting |

### Motion

| | Freya | egui | gpui |
| --- | --- | --- | --- |
| Primitive | `use_animation(\|_\| AnimNum::new(0., 100.).time(300).ease(Ease::Out).function(Function::Expo))`, `AnimColor`, `AnimSequential`; `anim.start()`, `.reverse()`, `.is_running()`, read with `anim.read()` | `ctx.animate_bool(id, open) -> f32`, `animate_bool_with_time(id, open, 0.15)`, `animate_value_with_time(id, target, secs)`, `animate_bool_with_easing`; `style.animation_time` default 0.1 s | `.with_animation(id, Animation::new(Duration::from_millis(200)).with_easing(ease_out_quint()), \|el, delta\| el.opacity(delta))`, `.repeat()`, `with_optional_animation` |
| Reactive transition | `use_animation_transition(value, \|from, to\| AnimColor::new(from, to).time(200))`, `use_animation_with_dependencies` | animate on the state bool each frame | re-render with a new animation id when the state changes |
| Easing | `Ease::{In, Out, InOut}` with `Function::{Linear, Quad, Cubic, Quart, Expo, Sine, Circ, Elastic, Back, Bounce}` | linear unless `animate_bool_with_easing` | `linear`, `quadratic`, `ease_in_out`, `ease_out_quint()`, `bounce(..)`, `pulsating_between(min, max)` |
| Interruptible | `.reverse()` mid-run retargets | per-frame target change retargets | restart with a new id; there is no retarget, so prefer short durations |
| Built-in animated widgets | `Accordion`, `Popup`, `Switch`, `Checkbox`, `Loader`, `AnimatedRouter` | `collapsing`, `Window`, scroll animation | none |

### Navigation and screens

| | Freya | egui | gpui |
| --- | --- | --- | --- |
| Routes | `#[derive(Routable, Clone, PartialEq)] enum Route { #[route("/")] Home, .. }`, `router::<Route>(..)`, `outlet::<Route>()`, `Link`, `ActivableRoute`, `AnimatedRouter`, `use_route()`, `RouterContext::get().push(..)` / `.replace(..)` | no router: a state enum (`enum Screen`, `Page`, `Tab`, `View`) matched in `update`; panels, `Window`, `Modal`, `menu_button` | root view holding `Entity<T>` children and an enum or `Option<Entity<..>>` for the active pane; `cx.open_window` for new windows |
| Where to grep | `Route::`, `#[route(`, `Link`, `.push(Route`, `.replace(Route` | `enum .*(Screen\|Page\|Tab\|View\|Mode)`, `self.screen =`, `.open(&mut` | `enum .*(Pane\|View\|Screen\|Mode)`, `cx.new(`, `open_window`, `actions!` |

### Theme tokens

| Freya | egui | gpui |
| --- | --- | --- |
| `get_theme()` / `use_theme()` from `freya-components`, `theme.corner_radius`, per component themes in `theming/component_themes.rs`, `Theme::new` | `ctx.set_style`, `Visuals`, `Widgets { noninteractive, inactive, hovered, active, open }`, `WidgetVisuals { bg_fill, weak_bg_fill, bg_stroke, fg_stroke, corner_radius, expansion }` | project-defined `Theme` behind `impl Global`, read with `cx.global::<Theme>()` or `cx.theme()` |

A literal color, size or radius beside an existing token is a finding. Report it once against the token.

## 3. Verification you can run

- `cargo check` / `cargo clippy` on the crate: confirms a proposed fix compiles. Never ship a `file:line` fix you did not type-check mentally against the signatures above.
- Freya: `freya-testing` `TestingRunner::new(app, size, ..)`, `runner.click_cursor(..)`, `runner.render_to_file("out.png")` for a snapshot; `features = ["devtools"]` for the live tree.
- egui: `egui_kittest` harness for snapshots and AccessKit queries.
- gpui: `#[gpui::test]` with `TestAppContext`.
- Visual judgment needs a render. Use the `run` skill to launch the app and screenshot it, or ask the user for a screenshot and the window size. A claim about rendered contrast, overlap or motion feel made from source alone is **Not verified**.

## 4. Web to Rust translation cheatsheet

| Web rule | Rust equivalent |
| --- | --- |
| `:focus-visible` ring | Freya `focus.is_focused()` style branch; egui native widgets already draw one; gpui `.focus_visible(..)` |
| `aria-label` on icon button | Freya `.a11y_role(AccessibilityRole::Button)` plus `.a11y_alt("Close")`; egui `widget_info(.. "Close")`; gpui mark **Not verified** |
| `<label for>` | egui `response.labelled_by(label.id)`; Freya `Input` component carries its own; gpui none |
| `prefers-reduced-motion` | a project-level `reduce_motion` setting; if none exists, report it once as a missing capability, not per animation |
| `transform: scale(0.97)` on press | Freya `.scale(0.97)` driven by a pressed state or `AnimNum`; egui `WidgetVisuals::expansion` on `active`; gpui `.active(\|s\| s.opacity(0.9))` since there is no scale transform on `Styled` |
| `@starting-style` entrance | Freya `use_animation` with `OnCreation` start policy; egui `animate_bool` seeded from the first frame; gpui `with_animation` on first render |
| `transition` vs `keyframes` | Freya `AnimNum` vs `AnimSequential`; egui `animate_*` vs manual timer; gpui single `Animation` vs `.repeat()` |
| `max-width: 65ch` | Freya `.max_width(Size::px(..))`; egui `ui.set_max_width(..)`; gpui `.max_w(px(..))` or `.max_w_2xl()` |
| `text-overflow: ellipsis` | Freya `.max_lines(1).text_overflow(TextOverflow::Ellipsis)`; egui `Label::truncate()`; gpui `.truncate()` / `.text_ellipsis()` |
| `tabular-nums` | font feature not exposed uniformly; use a monospace or tabular font family for changing numbers |
| Touch target 44px | Freya `.min_width(Size::px(44.)).min_height(..)`; egui `spacing.interact_size`; gpui `.min_w_11().min_h_11()` |
