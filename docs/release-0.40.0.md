# SeraBot 0.40.0: desktop chat, shell, and screens UX overhaul

This release delivers a comprehensive UX, accessibility, keyboard-navigation, and CSS polish pass across `crates/serabot-desktop`.

## Chat, composer, markdown, cards & approvals

- **Chat & composer (`ui/chat.rs`, `chat_compose.rs`, `chat_scroll.rs`, `chat_failure.rs`)**:
  - Unified slash picker actions, skills, and tools with keyboard indexing, `.active` highlight, `Tab`/`Enter`/`Escape` navigation, and IME composition guards across `@mention` and `/slash`.
  - Fixed `ModelBar` filtered search `.active` index and auto-applied egress-approved selections in compact chat mode with clear apply/approval states.
  - Added Mic disabled state when chat cannot accept turns, dynamic `composer_hint(enter_sends)`, `focus_composer()` on Edit, `Quote` (`> ...`) reply action, copy `"check"` feedback, non-empty draft confirmation on Regenerate, dismissible error/held-draft/transcript banners, and non-user `unseen` counting.
  - Cleared `pending` scroll restore on `__serabotPinMessages(true)` and `__serabotJumpToLatest()` so force-pins and Jump to Latest are never overridden by pending timers.
  - Mapped HTTP `401`/`403` to `ChatAction::Reconnect`, `404`/`413` to friendly copy, and filtered HTML error pages from raw payloads.
- **Markdown (`ui/markdown.rs`)**:
  - Added ordered-list paragraph breaks, loose unordered lists, `+ ` bullets, `- [ ]` / `- [x]` task lists, `~~~~` and N-backtick fences, pipe/code-aware table splitting, bounded FIFO cache eviction (`MD_CACHE_MAX = 384`), `~~strikethrough~~`, snake_case-safe `_italic_`, `***bold italic***`, backslash escapes, nested inline spans, safe `https://` bare autolinks via `chat_links::safe_chat_link`, and code-block line count + copy feedback.
- **Approvals, conversations & interactive cards (`ui/approvals.rs`, `ui/conversations.rs`, `ui_cards.rs`, `chat_cards.rs`)**:
  - Moved `Waiting for you` above `Standing grants`, added search filter, count chips, formatted + raw timestamps, truncated hash chip, per-row submit guard, and 2-step `Always` confirmation.
  - Added conversation search, `ConversationKind` filter pills, kind/parent/active/unread chips, `slugify_title`, `Enter`/`Escape` inline rename, and 2-step `Archive` confirmation.
  - Added `1..6` quick-pick shortcuts, IME guard, char-limit feedback, and submit cooldown on `QuestionPrompt`; `Enter`-to-submit and `eye`/`eye-off` toggle on `SecretPrompt` (preserving `Zeroizing`); and prompt context on resolved cards.

## Shell, screens, dock, widgets & CSS

- **Shell & navigation (`ui/shell.rs`)**:
  - Wired inline `RenameField` in `Topbar` when `header_rename` is active, added `IconRail` tooltips, populated `UserMenu` initial/label from `state.user`, added clear (`×`) and `Escape` on `#global-search`, auto-closed mobile `drawer_open` on selection, added dismiss (`×`) controls on `StatusBanner`, and surfaced `update.error` in `UpdateBanner`.
- **Tasks, Workflows & Logs (`ui_tasks.rs`, `ui_workflows.rs`, `workflows.rs`, `ui_logs.rs`, `logs.rs`)**:
  - Fixed `&due[..10]` slice panic via char-safe `format_due_date`; added tasks loading skeleton, search + status filters, `type="date"` + inline validation, overdue highlighting, notes line, 2-step delete confirmation, Kanban counts, and Calendar today/quick-add/`+N more`.
  - Added Workflows loading skeleton, search + `All`/`Enabled`/`Failing` filters, server-refresh sync for local drafts with URL validation, copy URL feedback, and clickable active rows.
  - Fixed Logs `"Clear filters"` query sync, added level tooltips, visible/total count pill, `Escape`-to-clear, Wrap toggle, visible/per-row Copy, Latest jump, and loading skeleton.
- **Files, Jobs, Bots, Dock & Widgets (`ui_files.rs`, `ui_jobs.rs`, `jobs.rs`, `jobs_reliability.rs`, `ui_bots.rs`, `ui_dock.rs`, `ui_widgets.rs`, `status_view.rs`, CSS)**:
  - Fixed undefined CSS variables (`--label` -> `--label-1`, `--err` -> `--danger`, Tasks/Browser vars), autofocused Lightbox + close button, stopped re-fetching preview on close, added `"Retry"` fallback, `file_icons::file_ext_image_src`, `"Show all (N)"`, and per-card busy indicator.
  - Preserved `ReliabilityAction.destination` + trimmed `last_error` with expandable `Details`, and prevented `JobsPanel` from fighting manual collapse.
  - Added inline avatar size validation, submit disable states, selected member chips + `"Clear all"` in `NewGroupSheet`, search + `"Chat"` in `MeetTeamSheet`, onboarding user initial + IME guard, and capability search filter.
  - Guarded `DOCK_JS` listeners, skipped `placePointer` work while closed, added `Escape`-to-cancel URL editing, Reload, Copy URL, `"Open full view"`, and Hide (`×`) in `.dock-head`.
  - Added `disabled`, `invalid` (`aria-invalid`), and `on_submit` to `SecretField`; `usage_tone`, `cpu_tone`, and `format_ratio_bytes` helpers; responsive `@container main` Kanban/Calendar grids and reduced-motion rules in `assets/*.css`.

## Verification

- `scripts\cargo-win.bat fmt --all -- --check`
- `scripts\cargo-win.bat clippy --workspace --all-targets -- -D warnings`
- `scripts\cargo-win.bat test --workspace --locked`
