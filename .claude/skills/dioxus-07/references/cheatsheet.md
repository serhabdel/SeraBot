# Dioxus 0.7 cheat sheet

Verified against the official 0.7 docs and the 0.7.10 source (see SOURCES.md). Everything here
is 0.7-valid; anything on the banned list is in `legacy.md`.

## 1. Cargo features and entry points

```toml
[dependencies]
dioxus = { version = "0.7", features = ["router"] }   # router is opt-in

[features]
default = ["web"]
web = ["dioxus/web"]
desktop = ["dioxus/desktop"]       # webview (wry). "mobile" is the same crate.
native = ["dioxus/native"]         # Blitz renderer, see the blitz-native skill
server = ["dioxus/server"]         # fullstack backend binary
```

Platform features exposed by the `dioxus` crate (0.7.10): `web`, `desktop`, `mobile`, `native`,
`liveview`, `server`, `fullstack`, `ssr`, `router`, `wasm-split`, `third-party-renderer`.
Default features: `launch`, `devtools`, `logger`, `lib`. `Store` ships with the `signals` feature.

```rust
use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);              // logger is auto-initialised by launch()
}

#[component]
fn App() -> Element {
    rsx! { "Hello, Dioxus!" }
}
```

`dioxus::LaunchBuilder::new().with_cfg(..).launch(App)` for per-platform config. Fullstack with a
custom Axum router: `dioxus::serve(|| async move { .. })` (see `fullstack.md`).

## 2. RSX

```rust
rsx! {
    div {
        class: "container",                       // attribute
        class: if active() { "is-active" },       // repeated class: merged; unterminated `if` allowed
        width: if wide { "100%" },                // conditional attribute
        background_color: "red",                  // inline style as snake_case attribute
        "data-id": "{id}",                        // quoted name = custom attribute
        ..extra_attrs,                            // spread; later wins
        "Hello {name}"                            // format-string interpolation
    }
    for item in items.iter() {                    // prefer `for` over iterator chains
        li { key: "{item.id}", "{item.title}" }   // stable key, NEVER the index
    }
    if logged_in() { Dashboard {} } else { Login {} }
    {children}                                    // expressions in braces
    {(0..5).map(|i| rsx! { span { "Item {i}" } })}
}
```

Escape hatches: `dangerous_inner_html: "{html}"` (XSS risk; never untrusted input),
`document::eval(..)` for JS (web and webview renderers; it never runs under the native renderer, see the
`blitz-native` skill), `web_sys` behind `#[cfg(feature = "web")]`, web components via a wrapper component
using a dashed tag name.

## 3. Components and props

- `#[component]` fn: Capitalised name or contains `_`; args are `Clone + PartialEq`; returns `Element`.
- Props are owned (`String`, `Vec<T>`), never `&str` / borrowed.
- `Element` is `Result<VNode, RenderError>`, so `?` works in components and handlers.
- `#[props(default)]`, `#[props(into)]`, `#[props(!optional)]`, `#[props(extends = GlobalAttributes)]`.
- `children: Element` receives child nodes; `EventHandler<T>` for callbacks, `Callback<In, Out>`
  when you need a return value; call with `.call(x)`.
- `ReadSignal<T>` makes a prop reactive and `Copy` (it accepts `Signal` and `Memo`). `ReadOnlySignal`
  is a deprecated alias since 0.7.0.
- Do not put `Rc<RefCell<..>>`, `Mutex` or `RwLock` in props; pass a `Signal` instead.
- Do not hand-write a `PartialEq` that always returns `true` (stale UI).

```rust
#[component]
fn Card(title: String, onclose: EventHandler<()>, children: Element) -> Element {
    rsx! {
        section { h2 { "{title}" } {children}
            button { onclick: move |_| onclose.call(()), "Close" } }
    }
}
```

## 4. State

| Need | API | Note |
|---|---|---|
| local state | `let mut n = use_signal(\|\| 0);` | read `n()` / `n.read()`; write `n.set(1)`, `*n.write() += 1`, `n += 1` |
| derived value | `use_memo(move \|\| n() * 2)` | re-runs when read signals change; `PartialEq` gates updates |
| side effect | `use_effect(move \|\| { .. })` | runs after render; use `.peek()` to read without subscribing |
| async value | `use_resource(move \|\| async move { .. })` | `Option<T>` while loading; not memoised; future must be cancel-safe |
| async + error boundary | `use_loader`, `use_action` | `use_action` cancels the previous run; the prelude exports `use_loader` (like `use_server_future`) only with the `fullstack` feature |
| one-time value | `use_hook(\|\| rand::random::<u64>())` | non-deterministic code goes here |
| share down tree | `use_context_provider(\|\| sig)` / `use_context::<Signal<T>>()` | bundle several signals in a `#[derive(Clone, Copy)]` struct |
| app-wide | `static S: GlobalSignal<T> = Signal::global(\|\| ..)` | isolated per app instance |
| structured fields | `#[derive(Store)]` + `use_store(\|\| ..)` | lens per field: `s.title().read()`, `.set(..)` |

`Store`, `use_store`, `ReadStore`, `WriteStore`, `GlobalStore` and the `#[store]` attribute are in
`dioxus::prelude::*` (0.7.10 prelude). Collections: prefer `Store<HashMap<K, V>>` so inserting or
removing queues one re-render, and pass items to children as `ReadStore<T>` / `ReadSignal<T>`.

Signal rules that panic when broken: `.read()` and `.write()` guards must not overlap; never hold
either across `.await`; a disposed signal panics when read. `Signal::new` outside a hook leaks
until its owner drops. `signal.toggle()`, `+=`, `.iter()` are available through extension traits.

Spawn work: async closures passed to event handlers are spawned automatically. `spawn(fut)` is
cancelled on unmount; `spawn_forever` attaches to the root. Tasks cancel on unmount, `.cancel()`,
or resource restart.

## 5. Events and forms

```rust
button { onclick: move |_| count += 1, "+" }
input { value: "{name}", oninput: move |e| name.set(e.value()) }
form { onsubmit: move |e| { e.prevent_default(); /* 0.7: forms submit by default */ } }
a { href: "..", onclick: move |e| e.prevent_default() }       // `prevent_default:` attribute is deprecated and does nothing
onclick: move |e| e.stop_propagation()
onclick: move |_| async move { let r = fetch().await; }       // auto-spawned
```

Handlers may return `Result` (`?` bubbles to the nearest `ErrorBoundary`).

## 6. Errors, suspense

```rust
ErrorBoundary { handle_error: |_| rsx! { "Something went wrong" }, Risky {} }
SuspenseBoundary { fallback: |_| rsx! { "Loading..." }, Child {} }
let data = use_resource(move || async move { .. }).suspend()?;   // client
let data = use_server_future(move || async move { .. })?;        // fullstack: serialised to client
```

## 7. Router (`features = ["router"]`)

```rust
#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[layout(NavBar)]
        #[route("/")]               Home {},
        #[route("/blog/:id")]       BlogPost { id: usize },     // dynamic segment: any FromStr
        #[route("/files/:..parts")] Files { parts: Vec<String> }, // catch-all, last segment
        #[route("/search?:q")]      Search { q: String },       // query segment
    #[end_layout]
    #[route("/:..route")]           NotFound { route: Vec<String> },
}

#[component] fn NavBar() -> Element { rsx! { Link { to: Route::Home {}, "Home" } Outlet::<Route> {} } }
#[component] fn App() -> Element { rsx! { Router::<Route> {} } }
```

Route component props must include every dynamic, catch-all, query and hash field of its variant.
Navigate in code with `let nav = navigator(); nav.push(Route::Home {});` (`replace`, `go_back`,
`go_forward`); `use_route::<Route>()` reads the current route. Match order: static, dynamic,
catch-all; nests cannot contain catch-all or query segments.

## 8. Assets, styling, Tailwind

```rust
const MAIN_CSS: Asset = asset!("/assets/main.css");   // path from crate root, leading "/"
rsx! {
    document::Stylesheet { href: MAIN_CSS }
    img { src: asset!("/assets/logo.png", AssetOptions::image().with_format(ImageFormat::Avif)) }
}
```

- Assets are content-hashed (`..-dxhx13xj2j.png`); disable with
  `AssetOptions::builder().with_hash_suffix(false)`. Unreferenced assets are dropped; use `#[used]`
  on a `static` to force inclusion. Files in `/public/` are copied as-is on web builds.
- `asset!`, `AssetOptions`, `ImageFormat` come from `dioxus::prelude::*` (it re-exports `manganis::*`
  under the `asset` feature, on by default). `manganis` also exports the attribute macro
  `#[css_module("/src/x.css")]` (used on a unit `struct Styles;`) and `option_asset!`.
- Read at runtime: `dioxus::asset_resolver::read_asset_bytes(&ASSET).await`.
- `document::Stylesheet`, `document::Link`, `document::Title`, `document::Meta` inject into `<head>`.
- **Tailwind (this is the one place it is documented; other skills link here).** Detection in the
  0.7.10 CLI (`tailwind.rs`): a `tailwind.config.js` or `tailwind.config.ts` next to the manifest selects
  Tailwind v3; otherwise a root `tailwind.css` selects v4. Content for v4:
  `@import "tailwindcss";` (plus `@source "./src/**/*.{rs,html,css}";` if needed). `dx serve` /
  `dx build` run the Tailwind CLI, write `assets/tailwind.css`, and you link it once with
  `document::Stylesheet { href: asset!("/assets/tailwind.css") }`. Custom tokens go in
  `tailwind.css` under `@theme { --color-brand: #00A8D6; }`.
  - **Download disclosure:** `dx` downloads the Tailwind binary from GitHub (no checksum is verified) and
    ignores a `tailwindcss` already on `PATH`, unless downloads are disabled with the `NO_DOWNLOADS`
    environment variable (any value) or the `no_downloads` CLI setting. Details in `tooling.md`.
  - The 0.7 Tailwind guide page still documents a manual flow with an npm-based Tailwind CLI (not used
    here), so the two official pages disagree; the `dx new` v0.7 template uses the root `tailwind.css`.
  - Class names written in Rust strings are scanned; repeated `class:` attributes help Tailwind see them.
- Theme tokens: CSS custom properties on `:root`. For the dx-components registry theme, see the
  `dioxus-design-system` skill.

## 9. Logging

`dioxus::launch` initialises logging (Debug in dev, Info in release). Manual:
`dioxus::logger::init(tracing::Level::INFO).expect("logger")` or `dioxus::logger::initialize_default()`.
Use `tracing::{debug, info, warn, error}!`.

## 10. Anti-patterns (official page, summarised)

Index keys; interior mutability in props; setting state in the render body (use `use_effect` /
`use_memo`); one giant state struct (split signals or use a Store); non-deterministic code in the
component body (use `use_hook`); an always-true `PartialEq` on props.
