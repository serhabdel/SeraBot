<!-- unverified: written from Iced 0.13 public docs, not checked against source. Confirm names against the project's pinned version before quoting. -->
# Iced map (Rust, Elm architecture)

Retained, Elm-style: `State`, `Message`, `update`, `view`, optional `subscription` and `theme`.

| Concept | Iced |
| --- | --- |
| Component | a struct with `fn view(&self) -> Element<Message>`; reusable pieces as functions returning `Element`; `iced::application(title, update, view)` |
| Conditional, lists | plain Rust `if`, `column(items.iter().map(..))`; keyed lists via `keyed_column` |
| Spacing | `column![..].spacing(8).padding(16)`, `row!`, `container(..).padding(Padding::new(..))`, `Space::with_height(..)` |
| Shape and surface | `container` style with `border: Border { radius: 8.into(), width: 1.0, color }`, `shadow: Shadow {..}`, `background` |
| Size | `.width(Length::Fill \| Fixed(px) \| Shrink \| FillPortion(n))`, `.height(..)`, `.max_width(..)` |
| Alignment | `.align_x(..)`, `.align_y(..)`, `center(..)`, `Alignment::Center` |
| Typography | `text("..").size(16).line_height(1.4).font(Font::..).wrapping(text::Wrapping::Word)`; `text::Shaping::Advanced` for non-Latin |
| Truncation | no built-in ellipsis; measure or wrap; keep full value in a tooltip |
| Hover, press, focus | widget styles take `Status::{Active, Hovered, Pressed, Disabled}` closures (`button::Style`); focus via `operation::focus`, `text_input::focus(id)` |
| Click | `button("Save").on_press(Message::Save)`; no `on_press` means disabled look |
| Tooltip | `tooltip(content, "text", Position::Bottom)` |
| Keyboard | `keyboard::on_key_press(..)` subscription, `text_input::on_submit`, `widget::focus_next()` / `focus_previous()` operations for Tab |
| Accessibility | limited; `accesskit` integration experimental. Report names as Not verified beyond labels |
| Motion | no built-in tween; `iced_anim` crate or a `Subscription` with `time::every` driving an `f32` in state; `lilt` crate |
| Navigation | an `enum Screen` in state matched in `view`; `iced::window` for multi-window |
| Theme | `Theme::custom(..)` with a `Palette`, or custom `StyleSheet`-style closures per widget; `application(..).theme(..)` |
| Verification | `cargo check`; no snapshot harness in core; `iced_test` crate for simulated interaction |

Web cheatsheet: focus ring → `Status::Active` style with a border; aria-label → not exposed, mark Not verified; reduced motion → a state flag read by the tween driver.
