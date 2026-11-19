# Sources for `dioxus-07`

Verified against dx 0.7.10 (57d6794). Latest stable Dioxus at retrieval: 0.7.10 (released 2026-07-30).
The `main` branch is `0.8.0-alpha.3` (2026-10-16), so claims about internals cite the `v0.7.10` tag.
All pages retrieved **2026-10-18**. Short code samples in the skill are adapted from the sources below.

Licences: the Dioxus source repository is `MIT OR Apache-2.0` per its crate manifests (GitHub metadata
reports Apache-2.0). The documentation pages on dioxuslabs.com and the `docsite` repository declare no
licence. `dioxus-template` and `dioxus-ai` declare no licence; `dioxus-test` is MIT.

## Official documentation, Dioxus 0.7 (`https://dioxuslabs.com/learn/0.7/...`), no licence declared

| Page(s) | Supports |
|---|---|
| `/` , `/essentials/`, `/guides/`, `/migration/`, `/getting_started/` | URL map; install options; wasm target; platform prerequisites |
| `/llms.txt` | the "exists but contains only the intro page" finding in `tooling.md` |
| `/migration/to_07`, `/migration/to_06`, `/migration/to_05/` | `legacy.md` |
| `/essentials/ui/{rsx,components,attributes,conditional,escape,assets,styling,hotreload}` | cheatsheet sections 2, 3, 8; hot-reload notes |
| `/essentials/basics/{signals,collections,resources,hooks,context,effects,event_handlers,async,error_handling,suspense}` | cheatsheet sections 3 to 6; rules of hooks |
| `/essentials/router/{routes,navigation,layouts}` | cheatsheet section 7 |
| `/essentials/fullstack/{server_functions,axum,project_setup,native}` | `fullstack.md` |
| `/guides/tools/{configure,creating}` | `tooling.md` section 5 |
| `/guides/utilities/{logging,tailwind}` | cheatsheet sections 8, 9; Tailwind |
| `/guides/testing/web`, `/guides/tips/antipatterns` | `tooling.md` section 7; cheatsheet section 10 |
| `/guides/platforms/desktop` | boundary with the `blitz-native` skill |
| `/guides/depth/custom_renderer` and its markdown source `https://raw.githubusercontent.com/DioxusLabs/docsite/main/docs-src/0.7/src/guides/depth/custom_renderer.md` | `renderer.md` concepts and the drift table |

`https://dioxuslabs.com/learn/0.7/llms-full.txt` (about 467K tokens) is intentionally not used.

## Dioxus source, tag `v0.7.10` and `main` (`https://github.com/DioxusLabs/dioxus`), MIT OR Apache-2.0

| File | Supports |
|---|---|
| `AGENTS.md`, `notes/architecture/01-CORE.md`, `06-RENDERERS.md` (identical to `main` except CORE) | `renderer.md`; workspace map; 0.8 drift (`Properties::builder` vs `component_builder`, VNode mount state) |
| `packages/core/src/mutations.rs`, `virtual_dom.rs` | exact `WriteMutations`, `Mutation`, `Mutations` and `VirtualDom` signatures |
| `packages/dioxus/Cargo.toml`, `packages/dioxus/src/lib.rs` | feature table; prelude re-exports (`Store`, `manganis::*`, `DioxusRouterExt`, `serve`) |
| `packages/fullstack-server/src/server.rs`, `lib.rs`, `serverfn.rs` | `DioxusRouterExt::{register_server_functions, serve_static_assets, serve_api_application, serve_dioxus_application}` |
| `packages/router-macro/src/lib.rs` | `#[end_layout]`, `#[end_nest]` |
| `packages/manganis/manganis/src/lib.rs`, `packages/logger/src/lib.rs` | `#[css_module]`, `option_asset!`; `init` / `initialize_default` |
| `packages/html/src/attribute_groups.rs` | HTML-global `prevent_default` is `#[deprecated]`: "This attribute does nothing"; an SVG-group variant remains |
| `packages/cli/src/cli/create.rs` | `dx new` template branch rule (`v<major>.<minor>`) |
| `packages/cli/src/cli/autoformat.rs` | `dx fmt --all-code` uses prettyplease; check-mode logic |
| `packages/cli/src/cli/check.rs` | `dx check` is an rsx lint over `.rs` files (module doc: "Run linting against the user's codebase") |
| `packages/cli/src/tailwind.rs`, `packages/cli/src/settings.rs` | Tailwind v3 / v4 detection; unchecked binary download from GitHub; `NO_DOWNLOADS` and the `no_downloads` setting |
| `packages/fullstack-macro/src/lib.rs` | `protocol` argument parsed but unused (`_input_encoding`, `_output_encoding`) |
| `packages/document/src/document.rs` | `NoOpDocument::eval` (the fact is owned by the `blitz-native` skill) |
| `Cargo.toml` at `v0.7.10` and `main` | pins (`axum 0.8.4`, `wry 0.53.5`); `main` is `0.8.0-alpha.3` |
| `https://api.github.com/repos/DioxusLabs/dioxus/releases` | release tags and dates |
| `https://api.github.com/orgs/DioxusLabs/repos` | 70 repositories listed; none named `mcp`; `dioxus-test` and `dioxus-ai` exist |

## Other repositories and registries

| Source | Supports | Licence |
|---|---|---|
| `https://github.com/DioxusLabs/dioxus-template`, branch `v0.7`: tree, `Bare-Bones/AGENTS.md`, `clippy.toml`, `tailwind.css` | `tooling.md` section 5 | no licence declared |
| `https://github.com/DioxusLabs/dioxus-test` (README) | testing status | MIT |
| `https://github.com/DioxusLabs/dioxus-ai` (README) | existence and scope | no licence declared |
| `https://docs.rs/dioxus-signals/0.7.10/dioxus_signals/` (`type.ReadOnlySignal`) | "Deprecated since 0.7.0" | crate licence `MIT OR Apache-2.0` |
| `https://docs.rs/dioxus-hooks/0.7.10/dioxus_hooks/macro.to_owned.html` | `to_owned!` still exists | crate licence `MIT OR Apache-2.0` |

## Verification performed

Against dx 0.7.10 (57d6794) in a minimal crate: `dx --version`; `--help` for every subcommand named in
`tooling.md`; `dx fmt --file -`; `dx fmt --check` (file and project mode); `dx check`;
`dx serve --hotpatch --help` with an unknown-flag control. Not run: `dx new`, `dx serve`, `dx build`.

## Excluded on purpose

- The download-and-pipe-to-shell CLI installer one-liner shown on the Getting Started page and in the
  generated `AGENTS.md`: a remote-code-execution pattern, not reproduced.
- Generated `AGENTS.md` guidance where it conflicts with 0.7.10 (it pins `0.7.1` and uses `ReadOnlySignal`).
