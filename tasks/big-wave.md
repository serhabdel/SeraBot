# Big Wave — task list

Spec: `docs/specs/big-wave.md` (owner-approved 2026-11-09). Prior release:
`docs/specs/release-0.28.0.md`, `tasks/release-0.28.0.md`. Gate per chunk:
`scripts\cargo-win.bat check --workspace --all-targets` +
`scripts\cargo-win.bat test --workspace --locked`. WSL fmt + clippy `-D warnings`
before each wave commit. Push after each wave gate; **never merge without the
owner's OK**.

## Post-merge tracker (2026-11-10)

- **Wave A shipped** — merged to `main` via [PR #37](https://github.com/serhabdel/SeraBot/pull/37)
  (`origin/main` @ `a8157b4`, includes release tip `3208a1a`).
- **Wave B integration** — [PR #39](https://github.com/serhabdel/SeraBot/pull/39)
  merged (`d5b52f7`); B1/B2 on `main`. [PR #40](https://github.com/serhabdel/SeraBot/pull/40)
  merged B5/B6 (`a1c3d8a`). **B3/B4** landing via PR #41 (rebased onto `main`).
- **Wave C draft** — `feat/big-wave-c1` rebased onto `main` @ `7f1a1c4`
  (PR #38 was **closed** on GitHub; reopen or open a new PR when ready).
- **Worktree hygiene** — `H:/source/SeraBot-wt/big-wave-c1` still has an
  in-progress `git rebase` + dirty tree from parallel agents; use
  `H:/source/SeraBot-wt/rebase-c1-tmp` as the canonical rebased C1 checkout
  until that worktree is reset (`git rebase --abort` then `git reset --hard
  origin/feat/big-wave-c1`).

## Active workstreams (in progress)

These run in parallel for **0.29 / Wave A** visibility:

- [x] **Wave A backend APIs** — A1, A2, A3, A5, A6, A7 (`crates/serabot`).
- [x] **Desktop Workflows tab** — A4 (`crates/serabot-desktop`); blocked on
      A1–A3 response shapes.
- [ ] **Chat UX polish** — continues 0.28 D1 (streaming, WIP/reply cards) for
      demo-quality operator experience; not a redesign.
      (partial in tree: `working.rs` / `ui_files.rs` / chat UI tweaks; not a
      closed gate item).

---

## Wave A — See the machine (0.29.0)

Closes 0.28 **C2** read path: routines, source health, active plans, traces,
vault probe.

### Server (`crates/serabot`)

- [x] **A1** `GET /v1/routines` — `list_routines(user_id)` in `store.rs`; wire
      `api/routines.rs`; fields: name, enabled, next_run_at, last_run_for, config.
      - Acceptance: acting user only; isolation tests in `api/tests/`.
      - Verify: `cargo-win test -p serabot` (routines HTTP tests).
- [x] **A2** `GET /v1/monitoring/sources` — configured sources + last
      ok/error/items_seen from `source_runs` + `load_sources` for acting user.
      - Acceptance: no HTML bodies; member cannot read another user's runs.
- [x] **A3** `GET /v1/workflows/active` — incomplete plans per user (goal,
      done/total, updated_at).
      - Acceptance: empty when no plan or plan complete; UID-scoped.
- [x] **A5** `GET /v1/traces?conversation_id=&limit=` — ordered `tool.*` +
      approval events; hashes/destination/elapsed only.
      - Acceptance: **never** secret canonical args in JSON.
- [x] **A6** `POST /v1/vault/providers/{id}/test` — exit-code style probe;
      mirror `vault_providers` safety; no token/value echo.
      - Verify: `api/tests/vault_http.rs` (or existing vault test pattern).
      - Desktop: Test button + Connected vaults summary in `ui_vault.rs`.
- [x] **A7** Gateway `tracing` spans — `user_id`, `turn_id`, `tool` on tool
      path; optional Logs filter by tool name.
      - Acceptance: daily log file still redacted.
      - Done: structured `tracing::info!` fields on execute/dispatch/approved
        paths; Logs `q` matches `tool=…` (placeholder + ring filter test).

### Desktop (`crates/serabot-desktop`)

- [x] **A4** Workflows tab — routines countdown, source health rows, active
      plans; Refresh; styling aligned with Jobs/Logs.
      - Acceptance: usable without opening chat; calls A1–A3 (and A5 link optional).

### Wave A gate

- [x] **Shipped** — PR #37 merged to `main` (0.29.0 Wave A).
- [ ] A8 Phase gate: full workspace check + test green; desktop smoke (Workflows
      tab, one trace, vault test); owner demo script from spec two-week cut.
      - Automated gate: `cargo-win check` + `test --workspace --locked` green
        (2026-11-09). Desktop smoke + owner demo still outstanding.

---

## Wave B — Reliable circle (0.30.0)

_Base: Wave B B1–B2 on `main` via PR #39; B5–B6 via PR #40._

- [x] **B1** Per-user routine CRUD — `PUT /v1/routines`, member default
      conversation enqueue (PR #39).
- [x] **B2** Merge watch into routines UX — Workflows schedule panel
      (`workflows.rs`, `ui_workflows.rs`, PR #39).
- [x] **B3** Per-user source overlay (v1) — `GET/PATCH/POST/DELETE /v1/sources`;
      HTTP tests in `big_wave_b3_b4.rs` (PR #41).
- [x] **B4** Reliability enqueue rules — `reliability.rs` + migration `0033`;
      capped daily `reliability_enqueues` (PR #41).
- [x] **B5** Standing grants API + UI — list/revoke for acting user; audit revoke
      (PR #40).
- [x] **B6** Jobs panel stuck/unknown — uncertain tasks, `outcome_unknown`
      actions; link to A5 traces (PR #40).

### Wave B gate

- [ ] B7 Phase gate + commit + push; isolation + URL policy review for B3.

---

## Wave C — Deep ops (0.31.0)

- [ ] **C1** Browser console upgrades — profile picker, last host, idle TTL,
      sign-in helper status (`feat/big-wave-c1` @ `03bb523`, rebased on main).
- [ ] **C2** Vault accounts view — local + external refs + cards; domain filter;
      last used from audit.
- [ ] **C3** Autonomy presets — watcher/analyst prompt blocks reference
      Workflows + `watch_now` + `plan_update`; no new tools.
- [ ] **C4** Admin reliability dashboard — scheduler lag, claim failures,
      source staleness (counts only).
- [ ] **C5** Flutter Workflows + traces — read-only parity with desktop.

### Wave C gate

- [ ] C6 Phase gate + commit + push; `flutter analyze` / `flutter test` if C5 touched.

---

## Cross-wave extras (from spec theme 6)

- [ ] **E1** Quota near-limit surfacing in console (usage API exists).
- [ ] **E2** Mem0 provider — document + optional console toggle if enabled in config.
- [ ] **E3** Update `docs/README.md` index row for Big Wave spec when Wave A ships.

---

## Two-week cut tracker (Wave A only)

| Week | Target |
| --- | --- |
| **1** | A1, A2, A3, A6 + tests; spec/tasks in repo (this file). |
| **2** | A4, A5, A7 + Wave A gate + owner demo. |
