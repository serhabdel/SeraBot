# Release 0.50.0

Orchestration at scale. Every handoff, hold, task and management change is
scoped to the acting user's id; management changes go through the audited
fleet mutation path and never widen a bot's tools, skills, approvals, prompt or
egress consent. Specification: `docs/specs/v0.49/spec.md` section 4.

All tests use synthetic fixtures. No live provider or account was used, and
nothing in this release is verified against a real provider. The desktop
changes were **not visually verified on a device**: no screenshot review (light
or dark) of the chat, runs sidebar or composer was done, and contrast was
checked by computing ratios from the CSS tokens only.

## Highlights

- **Fleet mutation path** (`fleet_mutation.rs`): REST `PUT /v1/bots/{id}` and `/route` and the new tools share one validation, policy scan, catalog and egress check, and write a `bot.profile_changed` audit row with before and after values.
- **Management tools** (`tools_bots.rs`): `bot_list`, `bot_read`, `model_list`, `handoff_status` (reads) and `bot_set_route`, `bot_set_enabled`, `bot_set_delegates`, `subagent_retry`. `main` only, owner turns only; setters always need approval (no standing grant, no YOLO). See `approvals.md`.
- **Handoff ledger** (migration `0038_handoffs.sql`, `delegate.rs`): idempotent per-delegate rows written with the child task; `GET /v1/handoffs`, `POST /v1/handoffs/{id}/retry` and `/cancel`; `[users] child_daily_limit` (default 500).
- **Auto-resume** (`runtime/auto_resume.rs`): a child that fails for good becomes `failed_resumable` and main gets a server-written follow-up turn (ids and reason codes only). At most two resumes per handoff; refused when `effects_unknown`.
- **`needs_review`**: tasks flagged around an approved external effect go to review after lease recovery or failure instead of being replayed.
- **`waiting_model`**: a turn whose routes are all on hold is parked until the earliest hold expires (6 h cap), attempt not counted.
- **Compaction** (`runtime/compaction.rs`): at most two per turn, on a too-long refusal or about 75% of the window; `POST /v1/conversations/{id}/continue` continues after a limit.
- **Fairness**: `max_concurrent_turns` 1..=8 (code default 4, shipped `serabot.toml` 2), `max_turns_per_user`, `max_active_children`, per-account inflight semaphore; owner turns before subagent turns. `scripts/load-fixture.sh|bat` exercises it.
- **Events**: `handoff.started|completed|failed`, `model.failover`, `provider.hold_set|cleared`, `task.waiting_model` (see `events.md`).
- **Desktop**: virtualized chat feed and 16 ms event coalescing (C1, C2); runs sidebar, composer queue with steer and `@bot` mentions, failed-turn card, waiting-model banner, compaction row, shortcuts sheet (C3 to C5).
- **Motion and accessibility pass (C6)**: new surfaces (`runs.css`) use transitions only, so they can be interrupted; reduced motion (OS setting and the in-app switch) keeps opacity only; focus rings are explicit inside clipped panels; the success chip text was darkened to pass AA on the light panel (4.2:1 before); a forced-colors edge was added; the reduced-motion rule in `app.css` no longer fades every `.msg`, which would have flickered each time the virtualized feed remounted a row.

## Upgrade notes

- Migration `0038` runs on first start.
- Raising `max_concurrent_turns` above 2 on a host: run the load fixture there first (SQLite write contention).

## Not in this release / unverified

- No visual or on-device review of the desktop; no screenshots taken.
- No live provider verification of failover, holds or resume.
- The load fixture passed locally at 2, 4 and 8 workers; it was not run on a target host.
- `POST /v1/bots` still needs a restart before a new bot file is live.
