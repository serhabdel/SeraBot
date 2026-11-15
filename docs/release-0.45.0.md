# Release 0.45.0

Wave 8 OAuth, Proton Pass, Vault, Tasks, and Visual Workflow Studio improvements across `serabot` and `serabot-desktop`.

## Highlights

- **Server OAuth, Proton Pass, and Routines/Workflows API** (`crates/serabot/src/api/accounts.rs`, `crates/serabot/src/api/connectors.rs`, `crates/serabot/src/api/routines.rs`, `crates/serabot/src/api/tasks.rs`, `crates/serabot/src/api/workflows.rs`, `crates/serabot/src/config.rs`, `crates/serabot/src/onedrive.rs`, `crates/serabot/src/personal.rs`, `crates/serabot/src/vault_providers.rs`):
  - Delegated `/v1/auth/callback` to personal and OneDrive consent handlers when the callback `state` belongs to a pending connector flow.
  - Updated `/v1/connectors/{id}/test` and `/v1/admin/connectors/{id}/test` to probe per-user Google/personal connectors cleanly without leaking tokens or secrets.
  - Enabled Proton Pass (`protonpass_enabled = true`) by default in `VaultConfig` and added automatic `pass-cli login` PAT session fallback in `ProcessCommand`.
  - Extended `PUT /v1/routines/{name}` with optional `label` and `prompt` metadata and added `PUT /v1/workflows/active/{conversation_id}` for editing active multi-step workflow goals and checklist items.

- **Desktop Vault, Tasks, and API Client** (`crates/serabot-desktop/src/api.rs`, `crates/serabot-desktop/src/session.rs`, `crates/serabot-desktop/src/ui_tasks.rs`, `crates/serabot-desktop/src/ui_vault.rs`, `crates/serabot-desktop/src/worker.rs`):
  - Upgraded Vault and Tasks views with full task editing (`PATCH /v1/tasks/{id}`) and calendar/kanban polish.
  - Wired `put_routine_details` and `put_active_workflow` in the desktop API client and worker with schema parity against `serabot`.

- **Desktop Visual Workflow Studio** (`crates/serabot-desktop/src/ui_workflows.rs`, `crates/serabot-desktop/src/workflows.rs`, `crates/serabot-desktop/assets/jobs.css`):
  - Added routine detail editing (custom label and prompt) and active workflow checklist/goal updates in Visual Workflow Studio with dedicated styling in `assets/jobs.css`.
