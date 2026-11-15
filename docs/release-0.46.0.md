# Release 0.46.0

Wave 9 improvements across `serabot` and `serabot-desktop`: Vault CLI auto-install, Tasks HTTP 415 & unified Calendar sync, OneDrive folder visibility & local-folder clarity, stronger secret redaction & Composer Credential Guard, and ChatView search / create-task / starter chips.

## Highlights

- **Vault CLI Auto-Install & Security** (`crates/serabot/src/vault.rs`, `crates/serabot/src/vault_providers.rs`, `crates/serabot/src/api/vault.rs`, `crates/serabot/src/gateway.rs`, `crates/serabot/src/maker.rs`, `crates/serabot/src/local_relay.rs`):
  - Added automatic CLI discovery and installation support for Vault providers (`op`, `bw`, `pass-cli`, etc.) with clean status/error reporting.
  - Strengthened secret redaction across gateway, maker, local relay, and collaboration paths.

- **Tasks HTTP 415, Unified Calendar Sync & OneDrive Clarity** (`crates/serabot/src/api/tasks.rs`, `crates/serabot/src/api/connectors.rs`, `crates/serabot/src/onedrive.rs`, `crates/serabot/src/config.rs`, `crates/serabot-desktop/src/api.rs`, `crates/serabot-desktop/src/ui_tasks.rs`, `crates/serabot-desktop/src/ui_connectors.rs`, `crates/serabot-desktop/src/ui_settings_page.rs`, `crates/serabot-desktop/src/ui_vault.rs`):
  - Fixed Tasks HTTP 415 content-type handling, added unified Calendar sync across tasks/events, and refined Tasks layout & Vault header UI.
  - Improved OneDrive root/404 handling and folder visibility, and clarified local-folder configuration in Settings/Connectors.

- **ChatView UX & Composer Credential Guard** (`crates/serabot-desktop/src/ui/chat.rs`, `crates/serabot-desktop/src/chat_compose.rs`, `crates/serabot-desktop/src/ui_cards.rs`, `crates/serabot-desktop/assets/jobs.css`):
  - Added in-chat search, quick create-task action, and starter chips in ChatView.
  - Added Composer Credential Guard to warn/prevent accidental secret pasting, plus `/vault` command and `SecretPrompt` card upgrades.
