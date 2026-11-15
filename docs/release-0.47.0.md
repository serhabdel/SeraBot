# Release 0.47.0

Wave 10 release across `serabot`, `serabot-desktop`, and `serabot_mobile`: `jobfinder` built-in specialist bot, smart conversation titles & `conversation.renamed` SSE, inline approvals across desktop/mobile/Telegram, 1-click Browser Vault autofill, Vault provider sync & item listing, compiled-in default bot avatars, and mobile Tasks/Chat/Preview upgrades.

## Highlights

- **Built-in `jobfinder` Bot & Collaboration** (`bots/jobfinder.toml`, `bots/main.toml`, `prompts/jobfinder.txt`, `crates/serabot/src/job_tools.rs`, `crates/serabot/src/collaboration.rs`, `crates/serabot/assets/bots/jobfinder.png`):
  - Added the `jobfinder` specialist bot (with `job_search` and `job_dossier_checklist` tools) and wired it into `main` delegates.
  - Added `calendar_events`, `tasks_list`, and `tasks_create` to `SHARED_TOOLS` so every bot can read calendar events and list/create tasks.

- **Smart Conversation Titles & Inline Approvals** (`crates/serabot/src/api/conversations.rs`, `crates/serabot/src/runtime/turn_loop.rs`, `crates/serabot/src/telegram.rs`, `crates/serabot-desktop/src/ui_cards.rs`, `crates/serabot-desktop/src/session.rs`):
  - Added automatic smart conversation title generation and `conversation.renamed` SSE event broadcasting.
  - Added inline approval cards in Desktop ChatView (with local/global partitioning), Mobile `ChatScreen`, and Telegram inline keyboard callbacks.

- **Browser Vault Autofill & Provider Sync** (`crates/serabot/src/api/browser.rs`, `crates/serabot/src/api/vault.rs`, `crates/serabot/src/vault_providers.rs`, `crates/serabot-desktop/src/api/browser.rs`, `crates/serabot-desktop/src/api/panel.rs`, `crates/serabot-desktop/src/ui_browser.rs`, `crates/serabot-desktop/src/ui_dock.rs`, `crates/serabot-desktop/src/ui_vault.rs`):
  - Added `POST /v1/browser/autofill` for 1-click Vault credential autofill on the active HTTPS page in Desktop Browser & Dock.
  - Added `POST /v1/vault/providers/{id}/sync` and `GET /v1/vault/providers/{id}/items` with `synced_items` feedback and auto-sync on provider test/save.

- **Desktop Chat UX & Compiled-In Bot Avatars** (`crates/serabot-desktop/build.rs`, `crates/serabot-desktop/src/avatars.rs`, `crates/serabot-desktop/src/bot_chats.rs`, `crates/serabot-desktop/src/ui/shell.rs`, `crates/serabot-desktop/src/ui/row_menu.rs`, `crates/serabot-desktop/src/ui/sheets.rs`):
  - Embedded default bot PNGs at compile time in `build.rs` so bot avatars render immediately and never disappear on refresh/reconnect, plus `ImageSlot::Group` support.
  - Improved chat sidebar, row menus, sheets, and shell navigation.

- **Mobile (`serabot_mobile` 0.47.0+47)** (`apps/serabot_mobile/`):
  - Added `TasksScreen`, upgraded `ChatScreen` (inline approvals, message actions), `ChatsScreen`, `HomeScreen`, and `PreviewPane`, and subscribed to `conversation.renamed` SSE events.
