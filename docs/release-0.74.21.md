# Release `0.74.21`

## Summary

- **Windows CI Test Fix (`session.rs`)**
  - Updated `failed_providers_load_sets_section_load_error` to use a non-retryable `403 Forbidden` mock response (accepting up to 2 connections with an immediate unblock before `join()`), eliminating the 5-second retry backoff flake on `desktop (windows-latest)`.

- **Animated Bot Avatars & Living Presence (`working.rs`, `ui.rs`, `ui/shell.rs`, `ui/chat.rs`, `ui/wait_cards.rs`, `assets/system.css`)**
  - Added fine-grained `ActivityKind` states (`thinking`, `searching`, `browsing`, `coding`, `delegating`, `waiting_approval`, `done`) keyed from `turn.step` tools/labels and `turn.completed` (2s completion shimmer).
  - Added activity-keyed `.avatar[data-activity="..."]` CSS rings and animations (breathing halo, sonar pulse, coding pulse, delegating dashed ring, steady amber `waiting_approval` hold ring, and emerald `done` shimmer), all gated under `.root:not([data-motion="reduce"])` and `prefers-reduced-motion`.
  - Added a `from_bot → to_bot` `.handoff-pill` indicator to `LiveCardData::Delegating`.

- **Smarter Bot Work Timeline & Interactive Memory Pills (`turn_trace.rs`, `ui/chat_feed_view.rs`, `ui/chat.rs`, `assets/system.css`)**
  - Added `LiveTimeline` (`Recalled N memories → Tool step(s) → Drafting reply`) with per-step elapsed durations and `+N earlier` collapsing in `LiveCard` and `TurnTraceBlock`.
  - Replaced plain memory items with interactive `.memory-pill` chips (hover tooltip + click-to-open `Section::Memory`) and added a **"Pin to memory"** quick action in assistant `.msg-actions`.

## Verification

- Verified with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`, `inherit` security & code review, and `cargo test -p serabot-desktop --lib --locked` (plus `wait_cards::tests` under `--features ui-check`). Live visual verification in desktop WebView2 remains for owner verification.
