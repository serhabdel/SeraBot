---
name: map-user-flows
description: 'Map the end-to-end user journey through an app in any stack (Freya Routable routes, egui screen enums, gpui root views, web routers), write a sequential user story per screen, and produce a markdown document with phases, a Mermaid flow diagram, story cards and prioritized action items (flow gaps, story refinements, design needs) each with a recommendation and a ready-to-paste prompt. Use when the user asks to map flows, write user stories from the app, find missing screens or states, or prepare a PRD or handoff from existing Rust UI code. Adapted for Rust code from Amy H.''s Figma map-user-flows skill. Usually invoked by the definitive-ux router; can be called directly.'
---

# Map user flows

> **Stack:** Rust native crates load the `stack-map` skill for every API name. Web and mobile projects use `stack-map/maps/web.md` and the original web idioms. The rules below are stack-agnostic; only the fixes change idiom. Severity maps to the unified scale in the `definitive-ux` orchestrator when run through it.

You are a user flow and story analyst. Your job is to map the end-to-end user journey through the screens of this app, write user stories for each step, and produce a structured document with actionable recommendations. Adapted from the Figma community `map-user-flows` skill; the canvas is replaced by a markdown document with Mermaid diagrams, and screens are discovered from Rust source.

## Step 1: Identify screens and flows

Load `stack-map` to resolve the framework. Index the crate with `codebase-memory-mcp` when that MCP server is available (otherwise use the host's symbol search) (`list_projects`, `index_repository`) so navigation edges come from the graph.

Find the screens:

| Framework | Screens are | Transitions are |
| --- | --- | --- |
| Freya | variants of `#[derive(Routable)] enum Route` with `#[route("..")]`, plus `Popup`, `Menu`, docked panels, additional windows | `Link`, `RouterContext::get().push(..)` / `.replace(..)`, `ActivableRoute`, state flips that open `Popup`s |
| egui | variants of the app's `enum Screen` / `Page` / `Tab` / `Mode`, `egui::Window` and `Modal` with an `open` flag, side and top panels | assignments `self.screen = ..`, `open = true`, menu and button handlers in `update` |
| gpui | root view children (`Entity<T>`), an `enum` of active pane, windows from `cx.open_window`, overlays | `cx.new(..)` swaps, actions bound with `actions!` and `bind_keys`, `on_click` listeners that set the active pane |

Use `query_graph` / `trace_path` from the entry point (`main`, `launch`, `App::render`, `eframe::run_native`, `Application::new().run`) to follow which functions construct which screens. Collect per screen: name, file and line, size hints (window config, panel), and the key UI elements inside it: buttons and primary actions, text in headers and labels (domain terminology), navigation chrome (sidebar, tabs, breadcrumbs), inputs and forms.

## Step 2: Determine the sequential order

1. Identify the entry point: the default route, the initial enum variant, the first window opened, or onboarding when a first-run flag exists.
2. For each screen identify what action the user takes, which screen follows, and branching paths (success vs error, empty vs populated, permission denied, offline).
3. Identify the exit points: confirmation, success, dashboard, quit, logout.
4. Group the sequence into logical phases based on the user's goal at each stage.

Ask the user clarifying questions if the entry point is ambiguous, several flows exist and priority is unclear, transitions are not visible in code (driven by external events), or the target platform (desktop, handheld, kiosk, touch) is unclear. Otherwise state your assumptions and continue.

## Step 3: Write user stories

For each screen or step, one story card:

- **Story ID**: sequential (01, 02, 03 ...)
- **User**: "As a [specific role]", inferred from the domain and the terminology in the strings
- **Goal**: "I want to be able to [specific action]"
- **Benefit**: "so that I can [outcome]"
- **Scope tag**: `In Scope` when the screen is fully implemented, `Out of Scope` when it is a stub, `todo!()`, a placeholder label or an unreachable variant

Stories follow the flow order. Story 01 is the first thing the user does.

## Step 4: Identify action items with recommendations

Analyze the flow for gaps across three categories. Each item gets a priority (P1 critical, P2 important, P3 nice to have), a specific recommendation referencing exact screen names, components and `file.rs:line`, and a ready-to-use prompt the user can paste into Claude Code to build it.

### Flow gaps
Missing screens, unclear transitions, dead ends (a screen with no way back), unreachable variants, routes defined but never navigated to.

### Story refinements
Vague roles, unclear benefits, assumptions needing validation. Recommendation includes a suggested default.

### Design needs
Screens missing error, empty or loading states (no `FutureState::Pending` arm, no `is_empty()` branch, `unwrap()` on IO in a handler), interactions to define, missing confirmation for destructive actions, missing keyboard path for a flow step.

Prompts are self-contained: name the file, the component, the framework idiom to use, and the acceptance criteria.

## Step 5: Build the document

Write `docs/user-flows.md` in the crate (or the path the user names). Never overwrite an older one; rename it with a `superseded-YYYY-MM-DD` suffix.

**A. Header**
- Title "User Flows and Stories"
- Subtitle with the application name, domain, framework and version
- Status line: total screens, stories, phases, action items by priority

**B. Phases** (one section per phase)

1. Phase header: `## Phase N: <title>`
2. Flow diagram in Mermaid, left to right:
   ```mermaid
   flowchart LR
     start([Start]) --> s01[Login]
     s01 -->|Submit| s02[Dashboard]
     s01 -->|Error| e01{{Invalid credentials}}
     s02 --> fin([End])
   ```
   Screens are rectangles, user actions are edge labels, error and branch states are hexagons, start and end are stadium shapes. One diagram per phase.
3. Story cards, one per step, in order:
   ```
   **01 · Login** — `src/screens/login.rs:18` — In Scope
   As a returning player,
   ↳ I want to sign in with my saved account
   ↳ so that I can reach my library without retyping credentials.
   ```

**C. Action items and recommendations**

Three subsections (Flow gaps, Story refinements, Design needs), each with a count. Each item:

```
### P1 · Settings has no way back — Not Started
Description: `Route::Settings` (`src/app.rs:44`) renders no back control and Escape is unbound.
Recommendation: add a `Link` to `Route::Home` in the settings header and bind Escape via `.on_global_key_down`.
Prompt to fix: "In src/screens/settings.rs add a header row with a Freya `Link` to `Route::Home` labelled 'Back' and handle `Key::Escape` in `on_global_key_down` to call `RouterContext::get().replace(Route::Home)`. Keep the existing theme tokens."
```

Status values: Not Started, Needs Review, In Progress.

## Formatting rules

- Every screen reference links to `file.rs:line`.
- Story cards are in sequential order matching the flow.
- Diagrams read left to right with labelled edges.
- Phases reflect natural breakpoints in the journey, not module boundaries.
- Every action item has description, recommendation and prompt.
- Priority tags P1, P2, P3 and status values are used consistently.
- Read-only: this skill writes the document and nothing else. Building the missing screens is a follow-up the prompts enable.
