# Release 0.56.0

Desktop composer & sidebar UI fixes, crisp WebView2 surface rendering, and
built-in fallback for the full 20-bot / 37-skill fleet on upgraded servers.

## Composer typing & caret stability

- **Stop erasing typed input:** replaced the `ChatView` composer sync effect
  (which subscribed to `composer_draft` and cleared every keystroke whenever
  `session.composer` stayed empty) with [`should_sync_external_composer`](../crates/serabot-desktop/src/chat_compose.rs),
  ensuring keystrokes and background session ticks never overwrite the active
  local draft while preserving conversation switches, failed-send restores, and
  submit clears.
- **Generation-tracked caret restoration:** updated `COMPOSER_CARET_SETUP_JS`
  and `COMPOSER_CARET_BRIDGE_JS` in `ui_files.rs` with `window.__serabotComposerGen`
  so delayed `requestAnimationFrame` caret restores never jump the cursor
  backward over newer typing during streaming repaints.

## Sidebar "New chat" menu & crisp WebView2 chrome

- **Fix sidebar New chat menu alignment & stacking:** aligned the sidebar
  `NewMenu` right (`right: true`) with explicit `r#type: "button"` triggers and
  menu items so `.new-chat-menu` opens inside the sidebar instead of extending
  under `.main` and `.topbar`.
- **Remove WebView2 blur fuzziness:** removed `backdrop-filter: blur(...)` from
  static desktop chrome (`.rail`, `.sidebar`, `.topbar.chat-header`, `.composer`,
  `.menu`, and card classes) in `assets/system.css`, stacked `.sidebar` at
  `z-index: 30` with an opaque background, sharpened `.btn.primary` with solid
  `--accent`, and updated user message bubble selectors to `.msg.user .msg-body`.

## Full 20-bot & 37-skill fleet on binary-only server upgrades

- **Compiled-in fleet & skills fallback (`crates/serabot/src/shipped.rs`):**
  embedded all 20 `bots/*.toml` + `prompts/*.txt` manifests and all 37
  `skills/**/SKILL.md` packs via `include_str!`.
- **Disk-first merge:** `load_bots_with_builtin` and `SkillCatalog::load_with_builtin`
  keep any customized manifests/skills on disk while filling in any missing
  shipped bots or skills, so binary-only server upgrades expose all 20 shipped
  bots (including catalog-only bots) in `/v1/bots/catalog` and the Marketplace
  Bots tab instead of only the original 5.
