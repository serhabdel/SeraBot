# SeraBot 0.74.23

## Highlights

- **Desktop AI Provider Modal Fix (`system.css`, `ui.rs`)**: Fixed `.sheet` / `.sheet.open` stacking (`z-index: 220` above `.scrim.open` at `z-index: 210`) and disabled `backdrop-filter` blur on modal sheets so the Add/Configure AI Provider dialog (and all `.sheet` dialogs) renders crisply and no longer dismisses when clicked.
- **Proton Pass Vault Connector Overhaul (`vault_providers.rs`, `api/vault.rs`, `vault_import.rs`, `ui_vault.rs`)**:
  - **Native `pass://` reads**: `read_args` now invokes `pass-cli item view <pass://...>` directly so both synced ID references (`pass://<share_id>/<item_id>/password`) and name-based references resolve cleanly.
  - **Headless server compatibility**: Passes `PROTON_PASS_KEY_PROVIDER=fs` and isolates `PROTON_PASS_SESSION_DIR` inside the per-call `0700` `TempDir` so `pass-cli` works on headless Linux servers without D-Bus/keyring.
  - **AI Agent Access Token audit reasons**: Automatically passes `PROTON_PASS_AGENT_REASON` (e.g. `SeraBot browser autofill for <host> (entry: <name>)`) on probes, syncs, and autofills so Proton Pass AI Agent tokens work seamlessly and log every access in Proton Pass Agent Activity.
  - **Serialized single-session lifecycle**: Serializes `pass-cli` sessions process-wide, runs `pass-cli login` up front, batches multi-vault `item list` calls in one session, and runs best-effort `pass-cli logout --force` before wiping the temp directory.
  - **Token validation & safe diagnostics**: Normalizes and validates `pst_...::...` tokens on save (`PUT /v1/vault/providers/protonpass`), captures bounded `stderr` into `Zeroizing<String>` solely for safe failure classification (never logging or returning raw CLI output), and filters out non-Login items during sync.
  - **Desktop local import & setup guide**: Updated `vault_import.rs` to list vaults then per-vault items with nested `pass-cli` JSON parsing, and added a collapsible Proton Pass AI Agent token setup guide in `ui_vault.rs`.
