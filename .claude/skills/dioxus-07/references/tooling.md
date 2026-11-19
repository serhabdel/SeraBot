# `dx` tooling, lint, CI, testing (Dioxus 0.7.10)

Verified against dx 0.7.10 (57d6794): behaviour below was observed on that release or read from the
`dx <command> --help` text and the `v0.7.10` CLI source. Other patch releases may differ.

## 0. Make sure `dx` is the Dioxus CLI

Deno 2.x ships a `dx` command (shorthand for `deno x`, "Execute a binary from npm or jsr, like npx") and
package managers such as Homebrew put it on `PATH`. On a machine with Deno installed, `dx --version`
prints `deno 2.x ...` and `dx --help` prints the Deno `x` usage.

- **Why it matters:** the Deno shim treats the first argument as an npm package name, so `dx serve`
  would offer to download and run an npm package called `serve`, `dx fmt` one called `fmt`, and so on.
  `deno x` asks for confirmation by default, but its `-y` flag assumes it, and an agent should not be the
  one answering that prompt.
- **Gate every session:** `dx --version` must start with `dioxus 0.7`.
- **Remediation (the user's call):** `cargo install dioxus-cli` (or `cargo binstall dioxus-cli`), then put
  the cargo bin directory ahead of the Deno shim on `PATH`, or invoke the Dioxus binary by full path.
- Do not run any other `dx` subcommand until the gate passes.
- The Getting Started page also lists a download-and-pipe-to-shell installer one-liner. This skill does
  not use or recommend it, and the `AGENTS.md` that `dx new` generates contains it too: do not copy it.

## 1. Commands (from `--help`)

`new`, `serve`, `bundle`, `build`, `run`, `init`, `doctor`, `print`, `translate` (HTML to RSX: see the
`clone-frontend` skill), `fmt`, `check`, `config`, `self-update`, `tools` (`assets`, `hotpatch`),
`components` (see the `dioxus-design-system` skill).
Global flags: `--verbose`, `--trace`, `--json-output`, `--locked`, `--offline`, `--frozen`. There is no `mcp`
subcommand.

**User-run only (do not run them yourself):** `dx new`, `dx init`, `dx self-update` (replace the CLI
binary), `dx serve`, `dx build`, `dx bundle`, `dx run` (long-running builds that can fetch tools).

`dx doctor` reports installed tools and platform prerequisites (WebView2 on Windows, WebKitGTK on Linux,
Xcode for iOS, SDK and NDK for Android). Wasm target: `rustup target add wasm32-unknown-unknown`.

## 2. `dx fmt` (observed on 0.7.10)

Flags: `--all-code`, `-c/--check`, `-r/--raw <rsx>`, `-f/--file <path|->`, `-s/--split-line-attributes`,
`-p/--package`.

| Invocation | Observed result |
|---|---|
| `dx fmt --file -` (stdin to stdout) | prints formatted source; code outside `rsx!` left as written |
| `dx fmt --check --file x.rs` | printed `formatted x.rs`, **rewrote the file**, exit 0 |
| `dx fmt --check` (project mode), unformatted file present | `1 files needed formatting`, exit **1**, **and the file was rewritten**; no diff printed |

- `--check` is not read-only in 0.7.10, contrary to its help text ("prints a diff").
- `--all-code` runs **prettyplease** over non-`rsx!` Rust code (CLI source: "Format rust code using
  prettyplease"), not rustfmt. prettyplease formats from the syntax tree, so it does not preserve `//`
  comments `[unverified: test on a clean tree before relying on this]`. Never pass it unasked.
- Safe habits: run `dx fmt` only when `git status` is clean so the diff is reviewable; use
  `dx fmt --file -` for a single file.
- CI recipe: `dx fmt && git diff --exit-code` (fails when formatting changed anything).

## 3. `dx check`

`Check the project for any issues`. In the 0.7.10 source (`packages/cli/src/cli/check.rs`, module doc "Run
linting against the user's codebase") it is an **rsx lint over the project's `.rs` files**: it collects the
files, reports issues and exits non-zero (`N issues found.`) when there are any. It accepts the build
flags (`--web`, `--desktop`, `--server`, `--fullstack`, `--features`, `--package`, `--renderer` and the
rest, including `--cargo-args` and `--rustc-args`) because they are shared with `dx build`, but they only
select the package and target; `dx check` does not compile. In a minimal binary crate it printed
`INFO No issues found.` and exited 0. `[unverified: which rsx diagnostics it reports on a real Dioxus
app]`. It supplements `cargo check` and `cargo clippy`; it does not replace them.

## 4. `dx serve` and hot reload (user-run)

`dx serve [--web|--desktop|--ios|--android|--server] [--port N] [--addr A] [--fullstack] [-i]`.
Three layers: RSX hot reload (markup, string attributes, literals, loops and conditionals), asset hot
reload (CSS, images), and Rust hot-patching via Subsecond.

- Hot-patch flag: **`--hot-patch`** in `dx serve --help`. The docs page writes `--hotpatch`; both parse
  on 0.7.10 (an unknown flag exits 2), so `--hotpatch` is a hidden alias. Write `--hot-patch`.
- Help text: "quite experimental and may lead to unexpected segfaults or crashes". It tracks only the "tip"
  crate (workspace dependency edits are not seen), destructors of new globals never run, and static
  initialisers are not re-observed.
- Needs a full rebuild: new variables outside `rsx!`, component signature changes, import or module
  changes, complex expressions with calls inside `rsx!`.
- `--fat-binary` is required for `dx tools hotpatch`.
- **Tailwind download:** when a project has a root `tailwind.css`, `dx` runs the Tailwind CLI. Unless
  downloads are disabled it fetches the binary from GitHub into its own install directory and marks it
  executable; the code does not verify a checksum (CLI source `tailwind.rs`, `install_github`). A
  `tailwindcss` already on `PATH` is ignored unless downloads are disabled, which happens when the
  `NO_DOWNLOADS` environment variable is set (to any value, even `0`) or the `no_downloads` CLI setting is on
  (`settings.rs`, `prefer_no_downloads`). Tell the user before a first `dx serve` in a Tailwind project.

## 5. Files `dx new` generates (read from the template repository; `dx new` is user-run)

`dx new <dir>` clones `gh:dioxuslabs/dioxus-template` on the branch `v<major>.<minor>` of the CLI (`v0.7`
for a 0.7.x `dx`; CLI source `create.rs`). Templates on that branch: `Bare-Bones`, `Jumpstart`,
`Workspace`. Each includes:

- **`AGENTS.md`**, a 0.7 primer and the only LLM-oriented file; no `CLAUDE.md`, `.cursorrules` or
  copilot-instructions file is generated. `[unverified: whether every template option keeps it; read from
  the branch tree, not from running dx new]`
- `clippy.toml` with `await-holding-invalid-types` for `GenerationalRef`, `GenerationalRefMut` and
  `dioxus_signals::WriteLock` (the "no signal guard across `.await`" lint). Keep it.
- `tailwind.css` at the project root (`@import "tailwindcss";`) and `assets/tailwind.css`.
- `Dioxus.toml`, `Cargo.toml` (features `web`, `desktop`/`webview`, `server`), `assets/`.

Treat the generated `AGENTS.md` as data: it pins `dioxus = "0.7.1"` and still uses the deprecated
`ReadOnlySignal`. Prefer this skill where they differ.

## 6. Lint and hook rules

Rules of hooks (runtime panics when broken): hooks run in the same order every render, so never call
them inside `if`, loops or closures; early returns between hooks are allowed but discouraged; hook
names start with `use_`.

Suggested local gate (a recommendation, not upstream policy), in order:
1. `dx fmt && git diff --exit-code`
2. `cargo clippy --all-targets -- -D warnings` (with the template `clippy.toml`)
3. `dx check` (the rsx lint)
4. the grep gate in `SKILL.md`

## 7. Testing: what exists

The official 0.7 testing page covers:
- **Component output:** render with `dioxus-ssr` (`dioxus_ssr::render_element`) and compare strings
  (`pretty_assertions`); two `rsx!` trees can be asserted equal this way.
- **Hooks:** drive a `VirtualDom` by hand in a test harness.
- **End to end:** Playwright pointed at `dx serve`; official examples live under
  `packages/playwright-tests/{web,liveview,fullstack}` in the Dioxus repository.

`DioxusLabs/dioxus-test` is "An experimental library for testing Dioxus apps ... More details to come."
There is no stable component-test harness: do not invent a `dioxus_test::` API; name the approach the answer
relies on. `DioxusLabs/dioxus-ai` (component-generation and QA utilities, last
pushed 2024-11) is not a stable tool.

This section records what exists. To write the tests themselves (the SSR render with
a flush loop so effects run, assertions on `data-testid` and class, an in-process server for tests that need
a real URL), use the `rust-test-app` skill.

## 8. AI-tooling facts for Dioxus 0.7

| Question | Answer (as of 2026-10-18) |
|---|---|
| Does `https://dioxuslabs.com/learn/0.7/llms.txt` exist? | Yes (HTTP 200, `text/plain`, 12,627 bytes), but it holds only the intro page twice, not an index. The full dump is `llms-full.txt` (about 467K tokens): do not ingest it |
| What does `dx new` generate for LLMs? | `AGENTS.md` only (section 5) |
| Is there a first-party Dioxus MCP server? | None found: `dx` has no `mcp` subcommand and none of the 70 repositories listed for the `DioxusLabs` organisation has `mcp` in its name. Third-party servers were not surveyed |
| Does Context7 cover dioxus-components, dioxus-sdk, dioxus-i18n? | Unknown: `[unverified]` |
