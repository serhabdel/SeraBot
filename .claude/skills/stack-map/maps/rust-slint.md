<!-- verified: 2026-10-23 elements and accessible-* properties against slint-ui/slint master builtin_elements.rs and docs reference (v1.18.1); fluent std-widgets listed from the repo; easing curve names from the documentation. User projects pin 1.13 to 1.17. -->
# Slint map (Rust, desktop, Android, embedded, wasm)

Declarative `.slint` UI compiled into Rust; Rust side holds models and callbacks. The `.slint` layer is where almost every UX finding lives. Conventions per form factor in `platforms/`.

| Concept | Slint |
| --- | --- |
| Component | `export component Card inherits Rectangle { in property <string> title; callback clicked; ... }`; Rust: `slint::include_modules!()`, `let ui = App::new()?; ui.on_clicked(..); ui.set_title(..)` |
| Conditional, lists | `if cond : Rectangle { .. }`, `for item in model : Row { .. }` with `[]` or a `VecModel`/`ModelRc` from Rust |
| Layout | `VerticalLayout`, `HorizontalLayout`, `GridLayout` with `padding`, `padding-left/right/top/bottom`, `spacing`, `alignment: stretch \| center \| start \| end \| space-between \| space-around` |
| Size | `width`, `height`, `min-width`, `max-width`, `preferred-width`, `horizontal-stretch`, `vertical-stretch`; units `px`, `phx` (physical), `%`, `rem` |
| Shape and surface | `Rectangle { background; border-radius; border-width; border-color; drop-shadow-blur; drop-shadow-offset-x/y; drop-shadow-color; clip: true; opacity }` |
| Typography | `Text { font-size; font-weight; font-family; font-italic; letter-spacing; color; wrap: word-wrap \| char-wrap \| no-wrap; overflow: clip \| elide; horizontal-alignment; vertical-alignment }`; `rem` scales with the system setting |
| Truncation | `overflow: elide` with `wrap: no-wrap`; keep the full value reachable (tooltip or detail) |
| Interaction states | `states [ pressed when touch.pressed : { .. } hover when touch.has-hover : { .. } ]` with `transitions [ in pressed : { animate background { duration: 100ms; } } ]`; `TouchArea { clicked => { } pressed; has-hover; mouse-cursor: pointer; }` |
| Keyboard and focus | `FocusScope { key-pressed(event) => { if event.text == Key.Escape { .. } accept } has-focus; forward-focus: inner; }`; `enabled: false` removes from focus; Tab order follows declaration order |
| Accessibility | `accessible-role: button \| checkbox \| combobox \| list \| listitem \| slider \| spinbox \| tab \| tablist \| text \| textinput \| table \| image \| progressindicator \| groupbox \| ...`, `accessible-label`, `accessible-description`, `accessible-checked`, `accessible-enabled`, `accessible-value`, `accessible-action-default`; built on AccessKit; every custom control needs role plus label |
| Motion | `animate x, opacity { duration: 200ms; easing: ease-out; delay; iteration-count; direction: alternate; }`; easings `linear`, `ease`, `ease-in`, `ease-out`, `ease-in-out`, `ease-in-quad`...`ease-out-expo`, `cubic-bezier(a,b,c,d)`; property animations retarget automatically (interruptible); `Timer` element for ticks |
| Std widgets | `import { Button, CheckBox, ComboBox, DatePicker, GroupBox, LineEdit, Menu, ProgressIndicator, RadioGroup, ScrollView, ListView, StandardListView, Slider, SpinBox, Spinner, Switch, TableView, StandardTableView, TabWidget, TextEdit, TimePicker } from "std-widgets.slint"`; they carry roles, focus and keyboard |
| Navigation | an `in-out property <int> page` or an enum on the root switched with `if`; `PopupWindow` for transient surfaces (`close-policy`, `popup.show()`); `Dialog` with `StandardButton` |
| Theme | `global Theme { in property <color> accent: #..; in property <length> space-2: 8px; }` imported everywhere; widget chrome via `--style fluent \| material \| cupertino \| cosmic \| qt` (also `SLINT_STYLE`); dark/light via `ColorScheme` / `Palette.color-scheme` |
| i18n | `@tr("Save")`, `@tr("{n} items" \| "{n} items" % n)` with `.po` files |
| Embedded | `slint::platform` with `software-renderer`, no `std`, `phx` units, fonts embedded with `font-size` resolution, `Image` as `@image-url(..)` pre-rasterized; animations cheap only on opacity and position |
| Verification | `cargo check` compiles `.slint` (build.rs); `slint-viewer` for live preview; `slint-lsp`; screenshot tests via `slint::testing` |

Web cheatsheet: focus ring → a `has-focus` state on the control's border; aria-label → `accessible-label`; prefers-reduced-motion → a `global Settings { in property <bool> reduce-motion }` read in `animate` durations; hit area 44px → `min-width: 44px; min-height: 44px` on the `TouchArea` owner; tabular numbers → a tabular font family.
