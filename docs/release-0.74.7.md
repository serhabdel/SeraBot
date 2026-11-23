# Release 0.74.7

Patch batch on 0.74: design work and doc polish only — no runtime changes.

## Proposed spec: on-demand tool packs (`docs/specs/tool-packs.md`)

- The owner is considering loading tools on demand in regrouped packs instead
  of long per-bot tool lists. The spec (mirroring the LSP spec's
  "proposed, not approved" status) covers: a server-defined `TOOL_PACKS`
  table, two new always-granted read-only tools `pack_list` / `pack_load`,
  `packs = [...]` in `bots/*.toml` expanded at seed time, prompt/finish-rule
  changes, desktop editor checkboxes in phase 2, and the security invariants
  (gateway allowlist + approvals unchanged, destructive tools always ask,
  packs are static and disjoint, UID scoping untouched).
- First packs when approved (0.75): `mail` (migrating `hostinger`,
  `mailbox`, `inbox`) and `social` (`social_extract`, `fetch_url`).
- Needs the owner's explicit sign-off before any code, like
  `docs/specs/lsp-workspace.md`.

## Doc polish

- `docs/skills.md`: `mail` skill row added; `email-triage` and
  `messaging-etiquette` now list `hostinger` and `mailbox`.
- README: note pointing at the new spec (next to the LSP note).

## Housekeeping

- Workspace version 0.74.7 (`Cargo.toml`), mobile `0.74.7+81`
  (`apps/serabot_mobile/pubspec.yaml`), `tasks/todo.md`, `AGENTS.md`.

## Verification

- `cargo fmt --all -- --check` clean; `cargo clippy -p serabot -p
  serabot-desktop --features serabot-desktop/ui-check --all-targets
  -- -D warnings` clean (no code changes beyond the version bump).
- No tests affected (docs-only batch); CI runs the full suite anyway.
