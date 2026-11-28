# Release 0.74.32 — Fluid UI Motion, Stacked Toasts, Spotlight `Ctrl+K`, Global Live Recovery, Calendar 403 Diagnostics & Mobile Desktop Parity

Release `0.74.32` delivers a cross-platform UI/UX, motion, connection-resilience, and stability polish across **Server**, **Desktop**, and **Mobile**:

## Desktop (`crates/serabot-desktop`)

- **Fluid Motion & Tactile UI Choreography (`assets/system.css`, `ui/shell.rs`, `ui/settings_view.rs`)**:
  - Smooth GPU-composited `pane-enter` cross-fade + micro-slide (`160ms`) on non-Chat section switches and Settings tab changes while keeping `ChatView` mounted and warm.
  - Springy entry curves (`pop-enter`) on `.sheet`, `.sheet-palette`, `.model-picker-pop`, and `.user-menu-pop`, plus gentle `backdrop-filter: blur(4px)` on modal scrims.
  - Animated active indicator pills on `.rail-btn.on` and `.settings-nav-item.on`, tactile `:active` press scale on buttons, and a blinking accent caret on actively streaming assistant replies.
  - Strictly gated under `.root:not([data-motion="reduce"])` and `@media (prefers-reduced-motion: reduce)`.
- **Auto-Dismissing Multi-Toast Stack (`session.rs`, `ui/shell.rs`, `assets/system.css`)**:
  - Replaced single-string `toast` with a typed `toast_queue` (`ToastTone::Ok | Info | Warn | Error`, up to 3 stacked toasts, consecutive deduplication, optional `ToastAction` buttons).
  - Auto-dismisses (`4.5s` for `Ok`/`Info`, `7s` for `Warn`/`Error`) with a bottom countdown bar that pauses on hover.
- **Spotlight-Grade Fuzzy Command Palette (`Ctrl+K`) (`picker_search.rs`, `ui/sheets.rs`)**:
  - Added `Library`, all 16 `SettingsSection::ALL` entries with icons, power actions (*Refresh AI models & quotas*, *Reconnect live server stream*, *Toggle Reduce motion*, *Check for updates*), keyword aliases, recent jumps (`palette_recent`), and `<mark class="palette-hit">` character highlighting.
- **Global Live Connection Health & Instant Recovery (`events.rs`, `session.rs`, `ui/shell.rs`, `ui/chat.rs`)**:
  - Added `EventStream::reconnect_now()` (`Condvar` wake) to bypass backoff sleep immediately.
  - Moved the reconnecting banner into global `StatusBanner` across every workspace page with **Reconnect now** and **View logs**, plus a `"Live connection restored"` `Ok` toast on recovery.

## Server (`crates/serabot`)

- **Calendar `HTTP 403` Diagnostics & 5-Minute Cooldown (`api/mod.rs`, `api/tasks.rs`, `api/connectors.rs`)**:
  - `GET /v1/tasks/calendar` now emits structured `provider_warnings` (`provider`, `status_code`, `message`, `hint`) with actionable `google_calendar` 403/401 guidance.
  - Added a 5-minute per-`(user_id, provider)` negative cooldown cache for persistent `401`/`403` errors (cleared on OAuth reconnect/disconnect or task sync), eliminating repeated `warn!` log spam.
  - Surfaced inline warning banners with **Open Integrations** in both Desktop (`ui_tasks.rs`) and Mobile (`tasks_screen.dart`).

## Mobile (`apps/serabot_mobile`)

- **Desktop Parity & Motion Polish (`app_state.dart`, `screens/home_screen.dart`, `quick_jump.dart`, `screens/chats_screen.dart`, `screens/tasks_screen.dart`, `screens/settings_screen.dart`)**:
  - Added global `_ReconnectingBanner` in `HomeScreen` when `_app.seenLive && !_app.live` with 1-tap **Reconnect** and `"Live connection restored"` SnackBar.
  - Added smooth `FadeTransition` + `SlideTransition` tab transitions and `AnimatedSize` banners (respecting `MediaQuery.disableAnimationsOf(context)`).
  - Added Spotlight-style **Quick Jump** (`lib/quick_jump.dart`) accessible from `ChatsScreen` with fuzzy subsequence matching and match highlighting across tabs, sub-screens, bots, and conversations.
  - Added **AI Providers & Quotas** section in `SettingsScreen` (`GET /v1/providers` & `GET /v1/providers/{id}/accounts/{aid}/quota`) showing remaining quota percentages and reset labels.
  - Added `CalendarProviderWarning` / `CalendarEventsResult` parsing and inline warning cards in `TasksScreen` Calendar view.
