# Banned legacy patterns (0.5 / 0.6-era and removed or deprecated 0.7 items)

Never emit these in new code; rewrite them when you meet them. "Evidence" names the official migration
page (`learn/0.7/migration/to_05`, `to_06`, `to_07`) or another primary source. Verified against
dx 0.7.10 (57d6794) and the `v0.7.10` source tag.

## Gone since 0.5

| Banned | Replace with | Evidence |
|---|---|---|
| `fn app(cx: Scope) -> Element` / `Scope` / `ScopeState` | `fn app() -> Element`; hooks, context and `spawn` are called directly | to_05: "scope is no longer passed as an argument" |
| `cx.render(rsx!{..})`, `render!(..)` | return `rsx! { .. }` directly | to_05 |
| `use_state(cx, \|\| 0)`, `**state` | `let mut s = use_signal(\|\| 0); s()` | to_05 |
| `use_ref` | `use_signal` | to_05 |
| `use_future((dep,), move \|(dep,)\| async ..)` | `use_resource(\|\| async ..)`; dependencies are inferred from signal reads | to_05 |
| `'a` lifetime props / borrowed props | owned props (`String`, `Vec<T>`) | to_05 |

`to_owned!` is **not** banned: the macro still exists in `dioxus-hooks` 0.7.10. It is unnecessary for
`Signal`s (they are `Copy`, so a `move` closure can capture them directly) but remains valid for cloning
non-`Copy` values into a closure.

## Gone or deprecated since 0.6

| Banned | Replace with | Evidence |
|---|---|---|
| `prevent_default: "onclick"` attribute | `onclick: move \|e\| e.prevent_default()` | to_06 lists the attribute as removed. In 0.7.10 the HTML-global `prevent_default` still exists, is `#[deprecated]`, and its own note says "This attribute does nothing", so old code silently stops preventing the default. An SVG-group `prevent_default` also remains, documented as recommended only for LiveView (`attribute_groups.rs`) |
| `return None` from a component | `return VNode::empty()` or `rsx! {}` (`Element` is `Result<VNode, RenderError>`) | to_06 |
| `launch(app)` bare | `dioxus::launch(app)` | to_06 |
| `dioxus_logger::init(..)` as a separate dependency | built in: `dioxus::logger::init(level)` / `initialize_default()`; `launch` auto-inits | to_06 |
| free `eval("..")` | `document::eval("..")` | to_06 |
| `asset!(image("./assets/x.png").size(100, 100))` | `asset!("/assets/x.png", ..options..)` with a crate-root path | to_06 |
| feature `dioxus/axum` | `dioxus/server` | to_06 |
| `fullstack::Config`, `register_server_fns`, `Config::addr` | `LaunchBuilder::with_cfg`, `register_server_functions`, `PORT` / `IP` env vars | to_06 |

## Gone or changed in 0.7

| Banned | Replace with | Evidence |
|---|---|---|
| crate `dioxus-lib` / `use dioxus_lib::prelude::*` | `dioxus = { version = "0.7", default-features = false, features = ["lib"] }` and `use dioxus::prelude::*` | to_07 |
| `ImageAssetOptions::new().with_size(..)` | `AssetOptions::image().with_size(..)` | to_07 |
| form `onsubmit` that relied on auto-prevent | add `e.prevent_default()`; forms now submit by default | to_07 |
| `#[server(protocol = Http<Json, Json>)]` as the way to get JSON | plain `#[server]` (JSON is the default in 0.7) | to_07 |
| generic `server_fn::ServerFnError` | Dioxus's own `ServerFnError` (via the prelude) | to_07 |
| relying on the prelude for `use_drop`, `Runtime`, `queue_effect`, `provide_root_context` | import them explicitly | to_07 |
| `ReadOnlySignal<T>` | `ReadSignal<T>` | docs.rs `dioxus-signals` 0.7.10: "Deprecated since 0.7.0" |

## Custom-renderer legacy

0.6 removed `Template::name`, switched hot reload from `dioxus-hot-reload` to `dioxus_devtools`, and 0.7
changed event listeners to `impl SuperInto<ListenerCallback<$data>, __Marker>`. The 0.7 custom-renderer
guide text still shows `Template { name: .. }`, `rebuild()` returning mutations and `UserEvent`; see
`renderer.md` for the 0.7.10 signatures.

## Grep gate

The gate lives in one place: `SKILL.md`. It scans `src` and `Cargo.toml` for every row above (its
`use_future\((cx, )?\(` alternative matches the removed dependency-tuple form in both the `cx` and no-`cx`
spellings). A bare `use_future(|| ..)` is not flagged, because the migration page documents only the
deps-tuple removal `[unverified: whether bare use_future is still the recommended API in 0.7; prefer
use_resource]`. Zero matches means the gate passed, not that the code is correct: still run `cargo check`.
