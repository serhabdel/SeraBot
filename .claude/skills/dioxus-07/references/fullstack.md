# Fullstack, server functions and Axum (Dioxus 0.7)

Sources: 0.7 Fullstack pages (project_setup, server_functions, axum) and the `to_07` / `to_06`
migration pages. See SOURCES.md.

## Project setup

Fullstack builds **two binaries** (client and server). `dx` detects the `server` Cargo feature.

```toml
[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
tokio = { version = "1", features = ["full"], optional = true }   # server-only deps are optional

[features]
default = ["web"]
web = ["dioxus/web"]
server = ["dioxus/server", "dep:tokio"]
```

- `dx serve` enables the platform feature (`web` / `desktop` / `mobile`) for the client and `server`
  for the backend. Targets: `--web` (wasm32-unknown-unknown), `--desktop` (host), `--ios`.
- Guard server-only code with `#[cfg(feature = "server")]`; server functions compile only with it.
- Separate crates: `dx serve @client --bin dog-app @server --bin pet-api`.
- Native (desktop/mobile) apps render client-side only; call `set_server_url(..)` before requests
  because there is no default host origin.

## Server functions: attribute forms

`#[get("/path")]`, `#[post("/path")]`, `#[put]`, `#[delete]`, `#[patch]` declare an explicit HTTP
method. `#[server]` is the anonymous form (method chosen automatically). Requirements: `async fn`,
returns `Result<T, E>`, arguments are `Serialize + Deserialize` (or `IntoRequest + FromRequest`).

```rust
#[get("/api/hello-world")]
async fn hello_world() -> Result<String> { Ok("Hello world!".to_string()) }

// path params, then `?query` params; Axum route syntax
#[get("/api/products/{product}?color&quantity")]
async fn get_product(product: String, color: String, quantity: Option<i32>) -> Result<Vec<Product>> { .. }

// path + query + body
#[post("/api/photos/upload?name&rating")]
async fn upload_photo(name: String, rating: i32, image: FileStream) -> Result<i32> { .. }

// server-only extractors hoisted into the macro (FromRequestParts types)
#[post("/api/user/login", auth: auth::Session)]
pub async fn login() -> Result<()> { auth.login_user(2); Ok(()) }
```

Note: the official AGENTS.md shipped in the `dx new` v0.7 template shows the form
`#[post("/api/double/:path/&query")]` with `Result<i32, ServerFnError>`. The 0.7 Server Functions
page uses `{path}` and `?query`. **Prefer the documented page form**; the two differ.
`[unverified: whether both route syntaxes are accepted in 0.7.10]`.

Return types: any `Serialize + Deserialize`; or Axum-style `IntoResponse + FromResponse` such as
`Streaming<String>`, `Websocket` (with `WebsocketOptions` arg), `axum::response::Response`.
Client call: just `hello_world().await` from an async handler.

### Encoding (changed in 0.7)

Default server-fn codec is **JSON**. Before 0.7 it was URL-encoded form data.

```rust
#[server]                                          // 0.7: JSON in, JSON out
async fn my_json_function(arg: MyStruct) -> ServerFnResult<MyResponse> { .. }
```

A `protocol = ...` argument is accepted by the `#[server]` macro parser, but in 0.7.10 it has no effect:
the macro source keeps the encodings in `_input_encoding` / `_output_encoding` fields that are never read
(`packages/fullstack-macro/src/lib.rs`). The `to_07` migration page still shows
`#[server(protocol = Http<GetUrl, Json>)]`; do not rely on it. Choose the HTTP method with `#[get]`,
`#[post]`, `#[put]`, `#[delete]` or `#[patch]` instead, and do not write
`#[server(protocol = Http<Json, Json>)]` (JSON is already the default).

### Errors

- `use dioxus::prelude::*` brings a `Result<T>` (an `anyhow`-style alias) into scope; on the
  client it downcasts to `ServerFnError`.
- `ServerFnError` is **Dioxus-specific in 0.7** (was generic): import it from dioxus, not `server_fn`.
- Variants include `ServerError { message, code, details }` and `Request(RequestError)`.
- Custom error enum: `Serialize + Deserialize + AsStatusCode` (plus `thiserror::Error`).
- Helpers: `.or_unauthorized("..")?`, `.or_http_error(StatusCode::UNAUTHORIZED, "..")?`.

## Axum

- `dioxus::launch(app)` starts a default Axum server for you.
- `dioxus::serve(|| async move { .. })` is the customisation entry point:

```rust
fn main() {
    #[cfg(feature = "server")]
    dioxus::serve(|| async move {
        use dioxus::server::axum::routing::{get, post};
        let router = dioxus::server::router(app)           // static assets + SSR + server fns
            .route("/submit", post(|| async { "ok" }))     // custom routes win over SSR fallback
            .route("/about", get(|| async { "about" }));
        Ok(router)
    });

    #[cfg(not(feature = "server"))]
    dioxus::launch(app);
}
```

- Layers: `Router::layer` / `route_layer`, per-method, per-handler. Global state: `Extension`
  layers, `Lazy<T>` (async init, `Deref` access) or `LazyLock<T>` (sync).
- A hand-rolled Axum router registers server functions through the extension trait (re-exported by the
  prelude under the `server` feature; defined in `dioxus-server`):
  ```rust
  use dioxus::prelude::DioxusRouterExt;
  let router = axum::Router::new()
      .register_server_functions()                       // routes only
      .with_state(dioxus::server::FullstackState::headless());
  // or, full SSR app: .serve_dioxus_application(ServeConfig::new(), app)
  ```
  `serve_static_assets()` and `serve_api_application(cfg, app)` are the other methods on the trait
  (0.7.10 source and doc examples).
- 0.7 moved to **Axum 0.8** (`axum 0.8.4` in the 0.7.10 workspace).
- Fullstack `Config` was removed in 0.6; pass configs through `LaunchBuilder::with_cfg`
  (`server_only!`, `web!`, `desktop!`). Feature `dioxus/axum` became `dioxus/server`.

## Hydration rules

The first client render must match the server HTML.
- Use `use_server_future` (not `use_resource`) for data needed on first paint; it runs on the
  server, serialises, and the client reads it synchronously.
- Browser-only APIs (`localStorage`, `window`) go in `use_effect`, which runs after hydration.
- Streaming suspense: `ServeConfig::builder().enable_out_of_order_streaming()` via
  `LaunchBuilder::new().with_context(server_only! { .. })`.

## Not covered here

Auth sessions, websockets, forms, middleware and SSG each have a page under `essentials/fullstack/`
(`authentication`, `websockets`, `forms`, `middleware`, `static_site_generation`). Read the page; not
reproduced in this skill. Deploying the server to Cloudflare Workers is outside this skill.
