# SeraBot 0.41.0: desktop integrations, providers, settings, and vault UX overhaul

This release completes the desktop UX, accessibility, validation, and CSS polish across Integrations, Marketplace, Connectors, Composio, Providers, Settings, Usage, Vault, Browser, Memory, Sheets, Row Menus, and Previews.

## Integrations, Marketplace, Connectors, Composio, Providers & Connect

- **Integrations & Marketplace (`ui_integrations.rs`, `integrations.rs`, `ui_marketplace.rs`, `marketplace.rs`)**:
  - Kept `RUNTIME_CALLS_PLUGINS = false` and all marketplace safety limits intact while replacing placeholder-host technical jargon with plain-language status messaging.
  - Added `StatusFilter` (`All` / `Installed` / `Coming soon`), `filter_plugins_with_status`, `category_counts`, and connector `suite_of` matching.
  - Switched Refresh to active-tab-only refreshes (avoiding concurrent `pending` drops), added `.search-wrap` Clear/`Escape`, separated loading/error/empty-search states, and added two-step `Escape` handling in `PluginPage` and `ConnectorSheet`.
- **Connectors & Composio (`ui_connectors.rs`, `connector_card.rs`, `ui_composio.rs`, `composio_flow.rs`)**:
  - Added `CardState::tone` / `legacy_tone` + `StatusChip`; fixed `PersonalCard` per-connector testing indicator, `GoogleSetupCard` stale saved banner (`local_submitted`), and `LegacyConnectorCard` unsaved indicator + `Discard`.
  - Added `SecretField` show/hide + `Enter` submit + post-connect draft clear, `CopyUriRow` feedback, OAuth spinner / `"Check status"`, two-step destructive confirms, and `ComposioFlow` timeout / cancel / dismiss + search/refresh UI.
- **Providers, Connect, OAuth & Standing Grants (`ui/providers.rs`, `provider_view.rs`, `provider_presets.rs`, `ui/connect.rs`, `oauth.rs`, `standing_grants.rs`)**:
  - Added `DirectoryFilter` + search, `ProviderDraftCheck` / `validate_provider_draft`, `format_provider_test`, `human_kind`, `Connection::tone`, inline validation, preset Advanced fields, `StatusChip`, prefilled `Configure`, and collapsed `Token usage` without inline styles.
  - Added `Enter` submit, `aria-describedby`/alert roles, invite `Remove` toggle, and `Copied` feedback in `ui/connect.rs`; added `<meta charset="utf-8">` in `oauth.rs` callback pages; added `active_grants` / `grant_matches` / `grant_destination_label` / `short_hash` helpers in `standing_grants.rs`.

## Settings, Usage, Vault, Browser, Memory, Sheets, Row Menus, Previews & CSS

- **Settings & Usage (`ui_settings.rs`, `ui_settings_page.rs`, `ui/settings_view.rs`, `ui_usage.rs`, `local_folders.rs`, `assets/settings.css`, `assets/usage.css`)**:
  - Added `WebSearchRow` with dirty Save/Saved feedback and non-admin hint; `validate_turn_limits_draft` preserving invalid drafts + Retry; `classify_mem0_card` (Loading / Error+Retry / Unavailable / Available).
  - Added `LocalFoldersCard` filter, remove confirmation, and `local_folders::notice_is_error` classification; `BotScopePicker` Select all/Clear + restore memory; `Escape`/autofocus in `SettingsPanel` confirm; `SkillsSection` loading/empty/ID validation; `FilesTab` search/Clear; `PinnedCard` keyboard activation; `MembersTab` error/loading/counter; `settings_view.rs` sign-out confirm + Telegram Copy + Voice auto-load.
  - Added `ui_usage.rs` error+Retry, `chart_axis_summary`, `filter_sort_models` / `filter_sort_bots`, `Provider` column, and `ThisPcCard` Refresh.
  - Scaled `--fs-title` / `--fs-display` / `--fs-hero` under `[data-text]`, fixed undefined `--fs-body-1` -> `--fs-title` on `.health-value`, and added provider/search/copy/settings/usage CSS classes.
- **Vault, Browser, Memory, Sheets, Row Menus & Previews (`ui_vault.rs`, `vault_import.rs`, `ui_browser.rs`, `ui/memory.rs`, `ui/sheets.rs`, `ui/row_menu.rs`, `row_menu.rs`, `preview.rs`)**:
  - Vault `.input` styling, search/kind filter, empty-vs-filtered states, dirty discard & remove-token confirms, and `ImportPickerView` filter / Select all / none / `Escape`.
  - Browser `browser_error` banner, filter, `ProfilesSection` draft preservation, and `FocusView` `Escape` cancel & Go button.
  - Memory `stamp(...)` timestamps, `validate_memory_content`, `memory_empty_copy`, `Enter`/`Escape`/Clear, and Cancel on `Confirm delete`.
  - Sheets ARIA/disabled Create/`Enter`, loading disable, and rules counter; Row Menu/Switcher `next_menu_index` + Arrow/Home/End/`Escape` + bottom viewport flip; Previews rejecting empty-stem dotfiles (`.pdf`) and enforcing a safe raster `data:` MIME allowlist.

## Verification

- `scripts\cargo-win.bat fmt --all -- --check`
- `scripts\cargo-win.bat clippy --workspace --all-targets -- -D warnings`
- `scripts\cargo-win.bat test --workspace --locked`
