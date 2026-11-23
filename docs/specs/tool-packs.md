# Spec (proposed): on-demand tool packs

Status: **proposed, not approved**. No code yet; this is the design note the
owner asked for before the 0.75 tool-loading redesign. Like
`docs/specs/lsp-workspace.md`, it needs the owner's sign-off before any
implementation.

## Goal

Bots currently carry a long `tools = [...]` list in every `bots/*.toml`, and
every listed tool is advertised in the prompt on every turn. The owner wants
tools **grouped into packs that load on demand** per task, so a bot advertises
a handful of packs and pulls the exact tools in only when the task needs them
— the same pattern skills already use (`skill_list` / `skill_load`) and MCP
toolkits use (`composio_search_tools` / `composio_execute`).

## Current model (what exists today)

- Every tool is registered once at startup in the gateway (`ToolRegistry`,
  `crates/serabot/src/main.rs`); `tools::known_tool_names()` is the static
  catalogue.
- Per turn, the runtime builds the allowed set in `runtime/prompts.rs`:
  the bot's stored `tools` list **plus** `collaboration::SHARED_TOOLS`
  (search/fetch/memory/vault/onedrive/… granted to every bot) **plus**
  always-granted tools (memory_*, skill_*, plan, yolo, team tools,
  conversation_library, …).
- Approvals bind user + tool name + canonical args + destination + artifact
  hash + policy version; `DESTRUCTIVE_TOOLS` always require the owner.
- Untrusted-data rule: tool/model/source content never decides which tools
  run — the model may *request* a pack, but the pack table is server-defined.

## Proposal

### 1. Pack registry (server-defined, static)

A `TOOL_PACKS: &[(&str, &[&str])]` table next to `SHARED_TOOLS`, e.g.:

| pack | tools |
| --- | --- |
| `mail` | `mail_list`, `mail_read`, `mail_send`, `mail_folders`, `mail_search`, `mail_flag`, `mail_move`, `mail_delete` |
| `social` | `social_extract`, `fetch_url` |
| `search` | `tavily_search`, `firecrawl_search`, `firecrawl_scrape` |
| `office` | `docx_*`, `pptx_*`, `xlsx_*`, `document_index`, `rag_search`, … |
| `comms` | `telegram_*`, `whatsapp_*`, `ask_teammate` |
| `cloud` | `onedrive_*`, `gmail_*`, `outlook_*`, … |
| `projects` | `project_*`, `project_check` |

Rules: pack membership is a subset of `known_tool_names()`; a tool belongs to
at most one pack (enforced by a unit test); `SHARED_TOOLS` stays as-is
(always granted, not pack-loadable); destructive tools stay in
`DESTRUCTIVE_TOOLS` — loading a pack never removes the approval requirement.

### 2. Two new always-granted tools

- `pack_list` — returns the pack ids + one-line descriptions + tool counts
  (no args). Read-only.
- `pack_load { pack }` — validates the pack id against the registry, adds its
  tools to the current turn's allowed set, and returns the tool names now
  available. Loading is per-turn by default; optionally persisted per
  conversation (owner decision — persistence makes the next turn cheaper but
  can surprise; start per-turn only).

Both are granted like `skill_load`: always registered, in the always-granted
list, never approval-gated. The model is untrusted, so `pack_load` only ever
reads the static table — it cannot invent tools or read other users' state.

### 3. Bot manifests

`bots/*.toml` gains an optional `packs = ["mail", "social"]` alongside the
existing `tools` list. Semantics: the stored tool list is **expanded at seed
time** to `tools ∪ pack tools` (so existing stored rows, fleet-mutation
validation, and `validate_bot_tools` keep working unchanged), and the bot's
prompt/system text mentions the packs it can load on top. Migration: switch
`hostinger`, `mailbox`, `inbox` to `packs = ["mail"]` first; other bots keep
their explicit lists until phase 2.

### 4. Prompt/UX changes

- The turn prompt lists loaded tools as today, plus a one-line "Available
  packs: mail, social, … — load with pack_load" hint when the bot has packs.
- `FINISH_RULES` gains a rule like: if a needed tool is missing, call
  `pack_list`/`pack_load` (or `ask_teammate`) instead of refusing.
- Desktop bot editor: pack checkboxes next to the tool checklist; the Bots API
  exposes `packs` on each bot row.

### 5. Security invariants (unchanged, restated)

- All execution still goes through `gateway`: allowlist, approvals, policy
  scanning, audit. `pack_load` is not an execution path — it only widens the
  *advertised* set for the turn from a static table.
- Approval binding is by tool name + canonical args; loading a pack does not
  pre-approve anything. Destructive tools still always ask.
- Pack definitions live in code (`TOOL_PACKS`), never in user rows or tool
  output; a unit test asserts every pack tool is known and packs are disjoint.
- UID scoping is untouched: tools still receive `ToolContext.user_id`; packs
  carry no per-user data.

## Phases

1. **0.75 phase 1:** `TOOL_PACKS` table + `pack_list`/`pack_load` tools +
   prompt hint + finish rule + tests; migrate `hostinger`/`mailbox`/`inbox`
   to `packs = ["mail"]`; release notes. No desktop changes yet.
2. **0.75 phase 2:** desktop bot editor pack checkboxes, `/v1/bots` packs
   field, optional per-conversation pack persistence (owner decision).

## Risks & mitigations

- Model forgets to load a pack → `pack_list` is cheap and always granted;
  finish rules tell the bot to load or hand off (`ask_teammate`).
- Context growth when a pack is loaded → packs are small (≤ 10 tools); the
  turn trace shows what was loaded.
- Confusion between packs and skills → skills carry *guidance*, packs carry
  *tools*; the prompt separates them (`skill_load` vs `pack_load`).
