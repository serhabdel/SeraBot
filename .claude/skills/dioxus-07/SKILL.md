---
name: dioxus-07
description: >-
  Write, fix or review Dioxus 0.7 app code in Rust - rsx!, #[component], use_signal / use_memo / use_resource, Stores, the Routable router, asset!, Tailwind in a Dioxus app, #[server] / #[get] / #[post] server functions, dx serve / dx fmt, hot-patching, and writing a custom renderer (VirtualDom, WriteMutations). Fire on "dioxus", "rsx", "dx serve", or errors like `cx: Scope`, "cannot find function use_state", "render! not found", "already borrowed" signal panics. NOT for porting an existing web UI or HTML to Dioxus (use clone-frontend), NOT for dx-components registry components or their theme (use dioxus-design-system), NOT for the Blitz / dioxus-native renderer (use blitz-native), NOT for writing Dioxus component tests or axum router tests (use rust-test-app).
allowed-tools:
  - Edit(**/src/**/*.rs)
  - Bash(dx --version)
author: aRustyDev
version: 0.1.0
license: AGPL-3.0-only
---

# Dioxus 0.7

Pinned to **0.7.x** (verified against 0.7.10). 0.8 is alpha: no alpha APIs.
This skill grants no network tool and makes no requests (an approved `cargo check` may fetch crates and rewrite `Cargo.lock`). Use whatever docs tool your agent has; if a Context7 MCP is available, resolve the Dioxus 0.7 docs library first, else consult the page named in `references/SOURCES.md`. Never ingest `llms-full.txt` (~467K tokens).

## Preflight
1. `dx --version` must print `dioxus 0.7.x`. A `deno ...` banner means `dx` is Deno 2.x's npx-style shim: it offers to download and run an npm package named after the subcommand (with `-y`, it just runs it). Run no other `dx`; tell the user to `cargo install dioxus-cli` and put the cargo bin directory first on `PATH`.
2. User-run only: `dx new`, `dx init`, `dx self-update`, `dx serve`, `dx build`, `dx bundle`, `dx run`. With a root `tailwind.css`, `dx` downloads a Tailwind binary from GitHub (no checksum) unless `NO_DOWNLOADS` (any value) or the `no_downloads` setting is on; only then is `tailwindcss` from `PATH` used.
3. `dx fmt` rewrites files even with `--check` (as of 0.7.10): only on a clean `git status`, prefer `dx fmt --file -`, never `--all-code` unasked (can discard `//` comments).
4. Read `Cargo.toml` features (`web`, `desktop`, `server`, `fullstack`, `router`) before editing entry points.
5. html to rsx: see clone-frontend.

## Banned legacy (never emit; replacements in `references/legacy.md`)
`cx: Scope`, `cx.render`, `render!`, `use_state(cx, ..)`, `use_ref`, `use_future((deps), ..)`, `'a` props, `dioxus-lib`, `ReadOnlySignal`, `prevent_default: "onclick"`, `asset!(image(..))`, `ImageAssetOptions::new()`, `dioxus/axum` feature, `register_server_fns`, bare `eval(..)`.
The one gate: `grep -rnE 'cx: Scope|cx\.render|render!\(|use_state\(|use_ref\(|use_future\((cx, )?\(|dioxus-lib|dioxus_lib|ReadOnlySignal|prevent_default: "|ImageAssetOptions|"dioxus/axum"|register_server_fns' src Cargo.toml`
`to_owned!` still exists in 0.7: unneeded for Signals, valid for non-Copy clones.

## Output
Edits (in `src/`), `cargo check` result or "not run", grep gate clean, unconfirmed APIs named. Example for "cannot find function use_state, port to 0.7":

```rust
#[component]
fn Counter() -> Element {
    let mut count = use_signal(|| 0);
    rsx! { button { onclick: move |_| count += 1, "clicked {count}" } }
}
```
"Swapped `cx`, `use_state`, `cx.render` for a signal and a returned `rsx!`. Gate clean. `cargo check`: not run."

## References
- `references/cheatsheet.md` RSX, props, signals, Stores, router, assets, Tailwind
- `references/fullstack.md` server functions, Axum, hydration
- `references/legacy.md` banned list with evidence
- `references/tooling.md` dx flags, hot-patch, lint/CI, what the official testing page covers (writing component tests: rust-test-app)
- `references/renderer.md` VirtualDom and `WriteMutations`
- `references/SOURCES.md` pages, dates, licences

Re-check on upgrade to 0.8: re-run the grep gate and `dx --help`, and read the 0.8 migration notes when they are published.
