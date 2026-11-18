# Release 0.55.0

Bot-integrated Claude Agent (ACP), Antigravity streaming and parity fixes,
steadier and faster chat switching, and a Studio workspace refresh.

## Bot-integrated Claude Agent (ACP)

Claude Code is no longer a separate top-level tab in the icon rail; each bot
can now run in either **Server model route** mode or **Claude Code (Local ACP)**
mode, with local desktop folder support when working on files on your PC:

- **Per-bot integration:** toggle `Execution Engine` in `RouteEditor`, switch
  from `ModelBar` (`Claude Code · Local desktop (ACP)`) in 1:1 bot chats, or
  use the chat topbar switcher (`ClaudeAgentBotView`).
- **Per-bot ACP state:** separate ACP controllers, per-bot folder preferences
  (`claude_agent_bots.json`), per-bot persisted transcripts under
  `acp-transcripts/<bot_id>.json`, one-time persona context injection, cached
  Node.js probe, and a **Clear history** action.
- **Security boundary preserved:** ACP continues to run only on the Windows
  desktop through the pinned `@agentclientprotocol/claude-agent-acp` adapter
  in one allowed folder, with per-action **Allow** / **Decline** cards and no
  `"always"` option.

## Antigravity (`google-antigravity`) streaming & parity

- **Incremental SSE streaming:** `CloudCodeStreamReader` and chunk-by-chunk
  `response.bytes_stream()` handling in `llm.rs` (4 MiB cap, 120 s idle read
  timeout, split multi-byte UTF-8 safety, `FINISH_REASON_UNSPECIFIED` filtering).
- **Project & catalog parity:** runtime `loadCodeAssist` / `onboardUser`
  project discovery when `project_id` is empty, catalog fallback on HTTP 400,
  consistent `AGY_CLIENT_ID`, empty-turns fallback, and `cachedContentTokenCount`
  subtraction.

## Chat responsiveness & stability

- **No remount on chat switch:** `ChatView` stays mounted across conversation
  switches; `CHAT_SCROLL_BRIDGE_JS` and `feed_window_js` coordinate scroll and
  window resets on `data-conv` changes.
- **Off-UI-thread persistence & faster scope transitions:** `State::new_scope`
  uses `std::mem::take` instead of cloning, `LocalAgent::start_with_credentials`
  builds its client on its background thread, and `PersistWorker` handles disk
  and keyring writes off the UI thread.

## Studio workspace, Command Palette & Marketplace polish

- **Grouped IconRail & Command Palette:** rail items grouped into Work, Data,
  Connect, and System; `Ctrl+K` opens `CommandPaletteSheet` for quick
  navigation and actions; `ConnectView` shows the running version badge.
- **Glassmorphic surfaces & motion:** updated surfaces, `.rail-divider`, and
  `.sheet-palette` styling in `assets/system.css` and `assets/claude-code.css`,
  respecting reduced transparency and reduced motion.
- **Marketplace copy:** replaced the `Coming soon` filter chip with **Available**
  and updated placeholder-host messaging to `"Not live yet..."`.

## Not verified live

- Antigravity streaming and project discovery are verified by unit tests against
  synthetic fixtures, not against a live Google account.
- Bot-integrated Claude Code (ACP) is tested against a synthetic agent and
  desktop unit/UI-check tests; it has not run against the real adapter on a
  Windows device yet.
