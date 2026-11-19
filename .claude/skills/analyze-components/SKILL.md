---
name: analyze-components
description: 'Inventory and audit every UI component in a UI codebase (Rust: Freya Component impls, egui Widget/ui fns, gpui Render/RenderOnce views; web: React, Vue or Svelte components). Catalogs them with their props, states and call sites via the codebase-memory graph, flags orphaned components, near-duplicates, naming drift, hardcoded styles beside theme tokens and missing states, writes a markdown audit document, then fixes the issues the user approves by consolidating duplicates and swapping call sites. Use when a UI crate feels messy, before a design-system cleanup or a handoff, or when the user asks to list, audit or clean up components. Adapted for Rust code from Amy Ha''s Figma analyze-components skill. Usually invoked by the definitive-ux router; can be called directly.'
---

# Analyze components

> **Stack:** Rust native crates load the `stack-map` skill for every API name. Web and mobile projects use `stack-map/maps/web.md` and the original web idioms. The rules below are stack-agnostic; only the fixes change idiom. Severity maps to the unified scale in the `definitive-ux` orchestrator when run through it.

Audit every component used in a scope, build a structured audit document, then fix the issues found. Adapted from Amy Ha's Figma `analyze-components`. The Figma canvas layout contract is gone; the output is a markdown document and the analysis runs on the code graph.

## 1. Scope and inputs

Resolve scope once and record it in the document subtitle:

- A crate, module or directory named in the request.
- Otherwise the crate's UI modules (`ui/`, `components/`, `views/`, `screens/`, `widgets/`).

Load `stack-map` to resolve the framework. Then `list_projects` and `index_repository` from `codebase-memory-mcp` (default `moderate`). Every "who uses this" answer in this skill comes from `query_graph`, `search_code` or `trace_path`, never from grep alone.

## 2. What a component is

| Framework | Component definition | Props | States |
| --- | --- | --- | --- |
| Freya 0.5 | `impl Component for X` with `fn render(&self)`; stateless `fn x(..) -> impl IntoElement`; 0.4 projects: `#[component] fn X(..) -> Element` with `rsx!` | struct fields, builder setters (`pub fn on_press(mut self, ..)`), `Readable`/`Writable` props | `use_state` status enums, `.maybe` branches, `FutureState` arms |
| egui 0.32 | `impl egui::Widget for X`, `impl eframe::App`, any `fn x(ui: &mut Ui, ..)` that draws | fn args or struct fields | `Response` branches, `enabled` args, `Option` args |
| gpui 0.2 | `impl Render for X`, `#[derive(IntoElement)] struct X` + `impl RenderOnce`, `Entity<X>` views | struct fields, builder setters | `.when` branches, `hover`/`active`/`focus` closures, `disabled` fields |

Collect for each: name, path and line, kind (stateful or stateless), props, states handled (hover, focus, pressed, disabled, loading, empty, error), theme tokens read, literal colors or sizes, and call sites from the graph.

## 3. Issues to detect

| Issue | How to detect | Severity |
| --- | --- | --- |
| **Orphan** | zero callers in the graph and not exported from `lib.rs` for downstream crates; `pub` does not make it used | P2 |
| **Duplicate** | two components whose render bodies differ only by literals or a prop name (e.g. `PrimaryButton` and `AccentButton` with the same structure); or a local reimplementation of a `freya-components` / egui / project built-in | P1 if both are used, P2 otherwise |
| **Naming drift** | mixed conventions (`Btn`, `Button`, `ButtonPrimary`, `primary_button`); a name that lies about what it renders; snake_case types or CamelCase fns | P3 |
| **Hardcoded style** | a literal color, radius, font size or spacing beside an existing theme token (`get_theme()`, `Visuals`, `cx.theme()`) | P2, one row per token with every location |
| **Missing states** | an interactive component with no hover, focus, pressed or disabled branch; a data component with no loading, empty or error branch | P1 for focus and disabled, P2 otherwise |
| **Prop sprawl** | more than roughly 8 props or several booleans that are mutually exclusive and should be one enum | P3 |
| **Leaky layout** | a component that sets its own outer margin or absolute position so callers cannot place it | P2 |
| **Unkeyed lists** | dynamic lists rendering components without `.key(..)` (Freya) or `push_id` (egui) or stable `ElementId` (gpui) | P2 |

Wrap each per-component inspection so a malformed or macro-heavy file is recorded as an issue row ("could not analyze") instead of aborting the run.

## 4. Build the audit document

Write `docs/component-audit.md` in the crate (or the path the user names). Never overwrite a previous audit; rename the old one `component-audit-superseded-YYYY-MM-DD.md` unless the user asks to delete it.

Structure:

**A. Header**: title, crate and framework with version, scope, date, totals (components, stateful, stateless, orphans, duplicates, issues by priority).

**B. Overview stats**: a small table of counts per issue type.

**C. Inventory**: one table, sorted by module then name.

| Component | Kind | Path | Props | States | Tokens | Callers | Issues |
| --- | --- | --- | --- | --- | --- | --- | --- |

`Callers` is a count with the top three call sites as `file.rs:line`. `Issues` lists the issue codes from section 3.

**D. Issues**: one subsection per issue type, each item a card with:
1. Priority tag (P1, P2, P3) and title.
2. Description with every affected `file.rs:line`.
3. **Recommendation**: the specific change (keep `Button`, delete `Btn`, swap 4 call sites; extract `radius_md` token; add disabled branch).
4. **Prompt to fix**: a self-contained instruction the user can paste back into Claude Code to perform exactly that change, naming the files and symbols.

**E. Fix plan**: ordered list of the fixes you propose to apply, cheapest first (delete, reuse built-in, consolidate, extract token, add state).

## 5. Fix the issues

Only after the document is written and the user approves the plan (or asked for fixes up front):

1. **Orphans**: delete the component and its tests; run `cargo check`.
2. **Duplicates**: keep the better-named, more-used one; port missing props; swap every call site found by `query_graph`; delete the loser; `cargo check`.
3. **Hardcoded styles**: replace with the existing token; if the token is missing, add it to the theme module in the project's idiom.
4. **Missing states**: add the branch using the framework's pattern from `stack-map`.
5. **Naming drift**: rename with `rust-analyzer` LSP rename when available, else careful search and replace; `cargo check`.

Follow the `rust-engineering` skill for every edit. After the batch, run `detect_changes` or re-index, then re-run the inventory and append a "Fixed" column to the document.

## Rules

- Read-only until the document exists and the user approves. The audit is the deliverable; fixes are the follow-up.
- One root cause is one issue row, with every location listed. Never one row per occurrence.
- Do not invent components that "should" exist. Report what is there.
- Built-in toolkit components (`freya-components`, egui widgets) are the reference. A local copy of one is a duplicate unless it adds behavior the built-in lacks; say which behavior.
