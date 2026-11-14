# SeraBot 0.39.0: multi-user isolation, RAG recall, and operational hardening

This release closes remaining multi-user isolation, RAG recall, least-privilege API, and Windows filesystem hardening gaps across the server crate.

## Per-user isolation & reliability

- **Per-user scheduled & direct conversation isolation (`dir:main`)**:
  - `Store::default_scheduled_conversation_id` keeps `"dir:main"` when unoccupied or already owned by the acting user, and deterministically resolves/creates `dir:main-<16hex>` for additional users so scheduled routines and recovery runs never collide on a global `"dir:main"` primary key.
  - `reliability::enqueue_source_reliability_recoveries` now resolves conversations via `store.default_scheduled_conversation_id(&user_id)` instead of hardcoding `"dir:main"`.
  - `Runtime::bounded_completion_for` similarly isolates per-bot direct conversations when `dir:{bot}` is owned by another user.
- **Resilient scheduler loop**:
  - Replaced bare `?` error propagation in `main.rs`'s background scheduler loop (`due_routines`, `default_scheduled_conversation_id`, payload serialization, and `enqueue_task`) with `tracing::warn!` logging so transient errors or individual failing routines never terminate the scheduler task.
  - Updated the CLI `about` string to `"Small-circle invitation-only multi-user agent"`.
- **UID-scoped `kv` keys & `/v1/models` ownership**:
  - Scoped `thinking:{user_id}:{conversation_id}`, `model_selection:{user_id}:{conversation_id}`, and `deck_approved:{user_id}:{hash}` in `kv` (reading legacy unscoped conversation keys only when the acting user owns the conversation or is the admin for an uncreated legacy id).
  - `GET /v1/models` now checks ownership of `query.conversation_id` (returning `404` when owned by another user) before refreshing catalogs or reading conversation-scoped settings.

## RAG recall, monitoring, security & API cleanup

- **Per-user effective sources in `GET /v1/monitoring/sources`**:
  - Switched `list_monitoring_sources` to `sources::effective_sources_for_user`, returning per-user source overlays (`enabled`) and member RSS feeds (`own: true`) accurately while preserving URL visibility rules.
- **Multi-user vector recall (`SqliteRetriever::retrieve`)**:
  - Oversamples the global `chunk_vectors` KNN subquery (`knn_k = (limit.max(1) * 10).max(100)`) before filtering by `d.status = 'ready' AND d.user_id = ?3` and applying `LIMIT ?4`, preventing a large-corpus user from starving another user's vector recall.
- **Least-privilege `/v1/connectors` & `/v1/providers`**:
  - Non-admin callers receive `name: null` for server-level age secret identifiers and `default_inbox_id: null` on `agentmail`, while retaining `configured`, `enabled`, `health`, and `personal` capability flags needed by the UI.
- **Windows ACL hardening (`secrets::restrict_permissions`)**:
  - Implemented `#[cfg(windows)] restrict_permissions` using `whoami` + `icacls` (`/inheritance:r`, `/grant:r <current-user>:F`, `/grant:r *S-1-5-18:F`), falling back with a warning if unavailable.
- **Batched media jobs & RSS name validation**:
  - Added `Store::list_media_jobs_for_user(user_id, limit)` and wired `GET /v1/jobs` to use it instead of scanning conversations in an N+1 loop.
  - `POST /v1/sources/rss` (`add_member_rss`) now rejects empty, >128-character, or control-character `name` values with `422`.

## Documentation

- Updated `docs/README.md` (multi-user summary and `delegate` handoff ceiling of `16` matching `delegate.rs`) and `AGENTS.md` (marketplace/plugin runtime status).

## Verification

- `scripts\cargo-win.bat fmt --all -- --check`
- `scripts\cargo-win.bat clippy --workspace --all-targets -- -D warnings`
- `scripts\cargo-win.bat test --workspace --locked`
