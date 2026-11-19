<!-- verified for Leptos 0.8.6 (user's webs/main project, 2026-10-23); Yew and Sycamore sections unverified. Content rules are maps/web.md; this file covers the Rust framework layer. -->
# Rust web map: Leptos, Yew, Sycamore

These render real HTML, so every rule in `maps/web.md` applies unchanged. What changes is where state, events and components live.

## Leptos 0.8

| Concept | Leptos |
| --- | --- |
| Component | `#[component] pub fn Card(title: String) -> impl IntoView { view! { <div class="card">{title}</div> } }` |
| Conditional, lists | `<Show when=move \|\| cond() fallback=..>`, `<For each=.. key=\|item\| item.id children=..>` |
| State | `RwSignal`, `signal()`, `Memo`, `Resource` with `<Suspense>` and `<Transition>`; `Effect` for side effects |
| Events | `on:click=move \|_\| ..`, `on:keydown`, `on:input`, `prop:value` |
| Forms | `<ActionForm action=..>` server actions, `<Form>`; native inputs with labels |
| Accessibility | plain HTML attributes: `aria-label`, `role`, `<label for>`; islands keep semantics |
| Motion | CSS; `AnimatedShow` for enter/exit classes; `leptos-use` `use_transition`-style helpers |
| Navigation | `leptos_router`: `<Router><Routes><Route path=path!("/") view=Home/>`, `<A href>`, `use_navigate()` |
| i18n | `leptos-fluent` with `.ftl` files, `move_tr!` |
| Theme | CSS custom properties; `<Meta>`/`<Html>` from `leptos_meta` for `data-theme` |
| SSR | hydration mismatches are UX defects (flash of wrong theme, layout shift); `<Transition>` keeps old content during loads |
| Verification | `cargo leptos watch`, `cargo check --features ssr`, browser tooling |

## Yew (unverified)

`#[function_component]`, `html!`, `use_state`, `Callback`, `yew-router` with `#[derive(Routable)]`. Same HTML rules.

## Sycamore (unverified)

`view!`, fine-grained signals, `sycamore-router`. Same HTML rules.

## egui and Freya on wasm

They draw to a canvas, so `maps/web.md` accessibility does not apply: no DOM, screen readers get nothing unless AccessKit is wired. Report accessibility as a platform gap once, then review with `maps/rust-desktop-core.md`.
