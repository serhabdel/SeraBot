<!-- unverified: written from Dioxus 0.6 public docs, not checked against source. Content idioms are the web map's; this file covers the Dioxus-specific layer. -->
# Dioxus map (Rust, web and hybrid desktop/mobile)

React-like `rsx!` components rendering to HTML (web) or to a webview (desktop, mobile) or native via Blitz. The content layer follows `maps/web.md`; the shell follows `platforms/desktop.md` or `mobile.md` when it is a hybrid.

| Concept | Dioxus |
| --- | --- |
| Component | `#[component] fn Card(title: String) -> Element { rsx! { div { class: "card", "{title}" } } }` |
| Conditional, lists | `if cond { .. }` inside `rsx!`, `for item in items { li { key: "{item.id}", .. } }` |
| State | `use_signal`, `use_memo`, `use_effect`, `use_resource` for async with `Suspense` |
| Spacing, shape, type | CSS: classes, Tailwind, or `style:` attributes; tokens as CSS custom properties |
| States | CSS `:hover`, `:focus-visible`, `:active`; `disabled` attribute |
| Events | `onclick`, `onkeydown`, `oninput`, `onsubmit`; `prevent_default` |
| Accessibility | HTML semantics: `button`, `label r#for`, `aria_label`, `role`; same rules as the web map |
| Motion | CSS transitions and `@starting-style`; `dioxus-motion` crate for springs |
| Navigation | `dioxus-router`: `#[derive(Routable)] enum Route { #[route("/")] Home {} }`, `Link { to: Route::Home {} }`, `navigator().push(..)` |
| Theme | CSS custom properties; `data-theme` toggling |
| Hybrid shell | `dioxus::desktop::Config` with `WindowBuilder` (title, min size, decorations), menus via `muda` or the desktop crate's menu API; mobile via `dioxus-mobile` |
| Verification | `cargo check`, `dx serve`, `dx build --platform desktop`; browser tooling for the content |

Hybrid findings: a desktop build with no native menu, no standard shortcuts, or a custom titlebar that breaks dragging belongs to `platforms/desktop.md`, not to the content.
