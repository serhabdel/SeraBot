# Desktop redesign — Grok-class owner console

Status: **authoritative design + API contract**. Desktop and server work is split
against this document. When an implementation detail is not specified here, the
existing repository conventions win (`AGENTS.md`, `docs/orchestration.md`,
`docs/desktop-client.md`).

## 1. Intent

The owner console is the single control surface for one operator. It must feel
like a finished consumer product (Apple-level craft, Grok-level chat ergonomics),
not an operator tool wearing a web admin UI.

Three product pillars drive every decision below.

1. **Chat first.** The chat column is the product. Everything else is a sheet or
   a popover that never steals focus from the conversation.
2. **Groups are teams.** A group is a named room with an explicit team of bots
   (1–5). The owner picks the team; the team answers together and may talk to
   each other within a bounded exchange.
3. **Native, not integrated.** Voice, providers, connectors, and file handling
   work like native desktop features. No "the server must be enabled first"
   dead ends in the owner's face.

## 2. Scope

In scope: `crates/serabot-desktop` (the Windows console) and
`crates/serabot` (the server it drives).

Out of scope: the Flutter mobile client under `apps/serabot_mobile`. It is not
built, not extended, and not broken by this work. It may need a later API
compatibility pass, but no mobile file is touched here.

## 3. Design system

The console is a WebView2 shell rendered by Dioxus. There is no Node runtime and
no CSS framework: the design system is hand-written CSS in
`crates/serabot-desktop/assets/app.css` plus inline SVG icons.

### 3.1 Tokens

Define these as CSS custom properties on `:root` and `[data-theme="light"]`.
Every component consumes tokens; raw colors are not allowed in component rules.

| Token | Dark | Light | Use |
| --- | --- | --- | --- |
| `--bg` | `#0b0b0f` | `#f5f5f7` | window background |
| `--surface` | `#16161a` | `#ffffff` | panels, cards |
| `--surface-2` | `#1d1d22` | `#f0f0f3` | nested surfaces, hover fills |
| `--glass` | `rgba(22,22,26,.72)` | `rgba(255,255,255,.72)` | sidebar/header material |
| `--hairline` | `rgba(255,255,255,.09)` | `rgba(0,0,0,.09)` | 1px separators |
| `--label-1` | `#f5f5f7` | `#1d1d1f` | primary text |
| `--label-2` | `#a1a1a6` | `#6e6e73` | secondary text |
| `--label-3` | `#6e6e73` | `#86868b` | tertiary text |
| `--accent` | `#0a84ff` | `#0071e3` | selection, focus, primary action |
| `--accent-2` | `#bf5af2` | `#8944e0` | secondary accent (agent/AI) |
| `--danger` | `#ff453a` | `#d70015` | destructive |
| `--ok` | `#30d158` | `#008009` | success |
| `--warn` | `#ffd60a` | `#b25000` | needs attention |

Radii: `--r-xs 6px`, `--r-sm 10px`, `--r-md 14px`, `--r-lg 18px`, `--r-xl 24px`,
`--r-pill 999px`.

Shadows are two-layer and soft — no hard drop shadows:

```
--shadow-1: 0 1px 2px rgba(0,0,0,.18), 0 6px 18px rgba(0,0,0,.16);
--shadow-2: 0 2px 6px rgba(0,0,0,.22), 0 18px 48px rgba(0,0,0,.28);
```

Motion: `--ease: cubic-bezier(.32,.72,0,1)`, `--dur-1: 140ms`, `--dur-2: 240ms`.

### 3.2 Typography

Stack: `-apple-system, "SF Pro Text", "Segoe UI Variable Text", "Segoe UI",
system-ui, sans-serif`.

Scale: 11px (micro, tracked +.06em uppercase), 12px (caption), 13px (body
secondary), 14px (body), 17px (title), 22px (display), 28px (hero). Titles use
`-0.015em` tracking; hero uses `-0.025em`. Body text uses `line-height: 1.5`.
Message text renders at 14.5px with `line-height: 1.62` and preserves
whitespace.

### 3.3 Materials and depth

- The sidebar and the header are **glass**: `backdrop-filter: blur(28px)
  saturate(180%)`, a translucent `--glass` fill, and a bottom/right
  `--hairline` border. They float above the scrolling content.
- Cards are `--surface` with a `--hairline` border and `--shadow-1`. Hover
  raises elevation to `--shadow-2` over `--dur-2`.
- The composer is a floating capsule: `--r-xl` radius, `--surface` fill,
  `--shadow-2`, inset hairline, and it sits above the message list with a
  gradient scrim behind it so text scrolls under it.
- Focus is always visible: `:focus-visible` gets a 2px `--accent` ring with a
  2px offset. Never remove the ring.
- The light theme is a true light theme — white surfaces, `#f5f5f7` canvas,
  dark text — not the current warm paper theme.

### 3.4 Components

Reusable classes that the whole app shares (the exact names are fixed here so
independent modules can be written against them):

```
.app-shell, .glass, .sidebar, .sidebar-head, .sidebar-search, .nav-list,
.nav-item, .nav-item.active, .sidebar-foot, .user-chip
.topbar, .topbar-title, .topbar-actions, .icon-btn, .icon-btn.primary,
.pane, .pane-head, .pane-title, .pane-body, .pane-section
.card, .card-grid, .card-head, .card-title, .card-sub, .card-actions
.btn, .btn.primary, .btn.ghost, .btn.danger, .btn.sm, .btn.icon
.chip, .chip.ok, .chip.warn, .chip.danger, .chip.muted
.field, .field-label, .input, .select, .textarea, .field-row, .hint, .error-text
.composer, .composer-row, .composer-input, .composer-actions, .composer-mic
.thread-list, .thread, .thread.active, .thread-title, .thread-meta, .thread-dot
.msg, .msg.bot, .msg.user, .msg-avatar, .msg-head, .msg-name, .msg-time, .msg-body
.avatar, .avatar.lg, .avatar.sm, .avatar.bot, .avatar.user
.sheet, .sheet-head, .sheet-title, .sheet-body, .sheet-foot, .scrim
.menu, .menu-item, .menu-sep, .menu-label
.empty, .empty-icon, .empty-title, .empty-text
.toast, .toast.err, .banner, .banner.warn, .banner.err, .skeleton, .spinner
.avatar-stack, .team-chip, .team-chip.remove, .roster, .roster-row
```

### 3.5 Iconography

Inline SVG only; no image assets ship. Icons are 16/20/24px, `stroke-width`
1.6, `stroke="currentColor"`, `fill="none"`, `stroke-linecap="round"`. The
`Icon` component in `crates/serabot-desktop/src/ui.rs` is the single source.
Required icon names (add any more that the workstreams need): `chat`,
`search`, `plus`, `send`, `mic`, `paperclip`, `image`, `settings`, `users`,
`robot`, `check`, `x`, `chevron-down`, `chevron-right`, `dots`, `sparkles`,
`shield`, `folder`, `bolt`, `refresh`, `download`, `external`, `edit`,
`trash`, `moon`, `sun`, `list`, `grid`, `user`.

### 3.6 Layout

Three panes, macOS-style:

```
┌────────────┬──────────────────────────┬───────────────────────┐
│  sidebar   │  topbar + chat column    │  inspector (optional) │
│  260px     │  minmax(0,1fr)            │  300px                │
└────────────┴──────────────────────────┴───────────────────────┘
```

- **Sidebar** — glass, 260px, fixed. Top: brand mark, "New chat" primary
  button, search field. Middle: nav list (Chat, Fleet, Memory, Approvals,
  Providers, Connectors, Settings). Bottom: connection status chip and the
  owner chip. On widths < 1100px the inspector collapses into a sheet; below
  920px the sidebar becomes an overlay drawer.
- **Topbar** — glass, sticky. Left: conversation title + kind badge + team
  avatar stack. Right: model selector, appearance toggle, refresh.
- **Chat column** — max content width 760px, centered. Message list scrolls;
  composer pinned at the bottom.
- **Inspector** — contextual: bot profile, conversation settings, team
  management, provider details, connector details.

### 3.7 Chat ergonomics (Grok-like)

- Message rows are **flat, avatar-led rows**, not heavy bubbles: 32px avatar,
  name + time on one line, body below, hairline separation. The owner's own
  messages align right in a `--surface-2` capsule.
- Each assistant message shows the bot that produced it (avatar, display name,
  role, live state). A message from a peer bot inside a group is visually
  distinct: hairline accent-left border in the bot's own hue.
- **The "+" button** in the chat column opens a menu with: New direct chat,
  New group, New section, New bot. It is present in the composer row and in
  the sidebar head.
- Hovering a thread in the sidebar reveals inline actions (new message, rename,
  archive) without a context menu.
- Empty chat shows the roster of available bots and the team's roster for a
  group, with a one-tap "add to team" action.
- Long messages are never truncated; the chat column scrolls.
- Keyboard: `Enter` sends, `Shift+Enter` newline, `Cmd/Ctrl+K` focuses search,
  `Esc` closes the topmost sheet or menu.

### 3.8 Group / team UX

- A group is created from the "+" menu: name, optional parent section, and an
  initial team chosen from enabled bots (multi-select, 1–5).
- The group header shows the team as an avatar stack; clicking it opens the
  **Team sheet**: a list of members with role, live state, model, and a remove
  action; an "Add bot" picker listing all enabled bots with search; the
  current team is saved on change.
- The composer in a group shows a "Talking to: <team>" chip row that can be
  collapsed.
- The transcript distinguishes owner, orchestrator, and peer bot messages.

## 4. Server contract — team rooms

### 4.1 Model

A conversation of kind `room:` may carry a **team**: an ordered list of 1–5 bot
ids that participate in that room. `main` is always implicitly the orchestrator
and is never listed as a team member.

Persistence (new migration `0005_conversation_teams.sql`):

```sql
CREATE TABLE IF NOT EXISTS conversation_teams (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    members TEXT NOT NULL,
    mode TEXT NOT NULL DEFAULT 'selected',
    updated_at TEXT NOT NULL
);
```

`members` is a JSON array of bot ids, e.g. `["analyst","maker"]`. `mode` is
`selected` (the orchestrator chooses which members answer) or `all` (every
member answers every owner message in the room).

### 4.2 HTTP API

```
GET /v1/conversations/{id}/team
  200 {"conversation_id":"room:review",
       "mode":"selected",
       "members":[{"bot":"analyst","role":"specialist","enabled":true,"live":true}],
       "max_members":5}

PUT /v1/conversations/{id}/team
  body {"members":["analyst","maker","watcher"],"mode":"selected"}
  200 same shape as GET
```

Rules: owner token required; the conversation must exist and must be of kind
`room:` (any other kind → `400`); 1–5 unique members; every member must be an
enabled, loaded bot other than `main`; a disabled or unknown bot → `400` with
the offending id named; `mode` must be `selected` or `all`. Writes are audited
as `team.member_set` with the canonical member list and mode.

### 4.3 Turn behaviour in a room with a team

1. The owner posts to the room. The turn runs as `main` (orchestrator) exactly
   as today.
2. The orchestrator selects the bots that are needed to answer. Two tools:
   - `delegate` — unchanged; one bounded handoff to one specialist (legacy
     path, still the default outside groups).
   - `team_ask` — new; `{"to_bots":["analyst","maker"],"objective":"...",
     "model_class":"routine"}`. Only `main` may call it, only in a room with a
     team, at most one call per turn, and `to_bots` must be a subset of the
     room team. Every listed bot gets its own turn on the same conversation.
   - When the room mode is `all`, the runtime runs **every** team member on
     the owner's message without waiting for the orchestrator to select, and
     `team_ask` is not required. Mode `selected` (the default) keeps the
     orchestrator in charge of who speaks.
3. Each selected bot posts one assistant message labelled with its bot id, so
   the transcript shows several answers to one question.
4. **Peer exchange** — a bot in a team turn may call `team_reply`
   `{"to_bot":"maker","message":"..."}` to hand a specific question to one
   peer. `team_reply` is only available to bots taking part in a team turn.
   Bounds, all enforced in Rust:
   - at most **2** peer replies per bot per room turn;
   - at most **3** peer-reply rounds per room turn;
   - at most **8** team messages per room turn in total.
   A peer reply is a normal queued task on the same conversation; the target
   bot sees the room transcript (owner question + peer messages) and answers.
5. Every step is audited: `team.asked`, `team.replied`, `team.round_completed`,
   and `team.turn_completed`. Events are appended to the event stream so the
   desktop live-updates without polling.
6. A room without a team behaves exactly as today.

### 4.4 Desktop surface

- Team sheet on the group header (add/remove members, live state, roles).
- `team` endpoints are called by the desktop client through the normal
  owner-token path with the same 3-second request gap and the same
  unknown-outcome rules.
- Live updates: the event stream carries team events so an open group sheet
  refreshes when another client changes the team.

## 5. Server contract — native audio

### 5.1 Telegram voice

Current behavior saves the voice file to the inbox and asks the owner to run a
tool. New behavior:

1. When `audio.enabled` is on and the audio provider is configured, the server
   downloads the Telegram voice file, **transcribes it immediately**, and
   enqueues the transcript as the owner's message in the same chat. The turn
   then runs normally and the answer arrives as an ordinary reply.
2. The original audio file is still stored in the inbox for reference, and the
   reply notes it in one short line.
3. When audio is off or unconfigured, the owner gets one clear line explaining
   that voice is off and how to turn it on — not a stack of instructions about
   tool names and inbox paths.

Transcription must reuse the existing `crates/serabot/src/audio.rs` provider
client and the existing configured transcription model. No new provider, no new
secret.

### 5.2 Owner API

```
POST /v1/audio/transcribe
  headers: X-Audio-Filename: note.webm
  body: raw audio bytes (max 20 MiB)
  200 {"text":"transcribed text","filename":"note.webm","bytes":1234}
```

Rules: owner token; audio provider must be configured, otherwise `503` with a
message naming the missing setting; the body limit is the same
`DEFAULT_TELEGRAM_AUDIO_MAX_BYTES`; unknown audio extensions are rejected by
the existing extension allowlist; the response never echoes a secret.

The endpoint is registered by the server team in `crates/serabot/src/api.rs`
and implemented by calling the transcription function that the audio team adds
to `crates/serabot/src/audio.rs` with this exact signature:

```rust
pub async fn transcribe_audio_bytes(
    config: &Config,
    bytes: &[u8],
    filename: &str,
) -> Result<String>
```

### 5.3 Desktop voice capture

- A microphone button lives in the composer.
- Capture uses the WebView2 `MediaRecorder` API through Dioxus `document::eval`
  (no new Rust dependency, no native audio library).
- On stop, the recorded blob is base64-encoded in the webview, handed to Rust,
  and POSTed to `/v1/audio/transcribe`. The transcript is inserted into the
  composer as a normal editable message; the owner edits before sending.
- Failure paths (no microphone permission, unsupported codec, server error)
  show an inline, non-blocking message in the composer. Recording never blocks
  the chat thread.

## 6. Native providers and connectors

The server already exposes provider and connector administration. The desktop
must make them feel native:

- **Provider presets** — a "Add provider" sheet offers presets for
  OpenAI-compatible, Mistral, Groq, OpenRouter-compatible, Ollama / local
  OpenAI-compatible, and a fully custom entry. Choosing a preset fills base
  URL, model list, and secret name; the owner only pastes the key.
- **Custom providers** — any OpenAI-compatible base URL is accepted, validated
  by the existing `valid_provider_id` / model-id rules.
- **Connector cards** — each connector shows its state, a "Connect" action that
  opens the provider's authorization URL in the **system browser** (never an
  embedded webview), the redirect URI to paste back, and a masked field for the
  refresh token or client secret. Saving writes through the existing admin
  endpoint and is audited server-side.

## 7. Sandbox posture

The remote server stays the sandbox. The console must make that visible rather
than hide it:

- **Settings → Sandbox** shows the live policy posture from `/v1/health`:
  policy version, which tools require approval, whether the data-egress
  consent is granted, and whether a restart is pending.
- Every approval-gated tool is marked in the Fleet screen (already the case)
  and now also in the tool list of a bot profile.
- The desktop never bypasses the gateway: all tool calls happen server-side
  under the owner's token, and the client shows the approval decision path
  (once / always / deny).

## 8. Acceptance criteria

**Design system**

- The console renders light and dark with the token set in §3.1 and no raw
  colors outside `:root` / `[data-theme]` blocks.
- Sidebar and header are glass; composer is a floating capsule; focus rings
  are visible everywhere; the app is usable from 920px to 2560px wide.
- No CSS file exceeds 40 KB.

**Team rooms**

- `GET`/`PUT /v1/conversations/{id}/team` behave as specified, including the
  rejection cases.
- In a room with a team of three bots, one owner message produces the
  orchestrator reply plus the replies of the bots the orchestrator selected.
- A bot can address one peer with `team_reply`, the peer answers, and the
  exchange stops at the documented bounds.
- Legacy `delegate` behavior in direct and legacy conversations is unchanged.

**Native audio**

- A Telegram voice message is transcribed and answered without the owner
  running a tool.
- The desktop mic button produces a transcript that the owner can edit before
  sending.

**Providers / connectors**

- A new provider can be added from a preset with only a pasted key.
- A connector can be enabled and its refresh token stored from the desktop.

**Gates**

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo test --workspace --locked` pass in WSL.
- `cargo check -p serabot-desktop --all-targets --locked` passes on Windows MSVC.
- `cargo check -p serabot-desktop --features ui-check` passes in WSL.
- Existing tests are not deleted or weakened.

## 9. Related documents

- [Desktop client](desktop-client.md) — current client behavior (to be updated
  by the implementation).
- [Orchestration](orchestration.md) — handoff model (to be updated by the
  implementation).
- [Audio](audio.md) — provider configuration.
- `AGENTS.md` — product boundary and security invariants.
