# Big Wave — task list

Spec: `docs/specs/big-wave.md` (owner-approved 2026-11-09). Prior release:
`docs/specs/release-0.28.0.md`, `tasks/release-0.28.0.md`. Gate per chunk:
`scripts\cargo-win.bat check --workspace --all-targets` +
`scripts\cargo-win.bat test --workspace --locked`. WSL fmt + clippy `-D warnings`
before each wave commit. Push after each wave gate; **never merge without the
owner's OK**.

## Post-merge tracker (2026-11-10)

- **Wave A shipped** — [PR #37](https://github.com/serhabdel/SeraBot/pull/37)
  (`a8157b4` on `main`).
- **Wave B shipped to `main`** — integration [PR #39](https://github.com/serhabdel/SeraBot/pull/39)
  (`d5b52f7`), B5/B6 [PR #40](https://github.com/serhabdel/SeraBot/pull/40)
  (`a1c3d8a`), B3/B4 [PR #41](https://github.com/serhabdel/SeraBot/pull/41)
  (`6cef441`). `main` @ `d62e869` after chat merge.
- **Wave C C1** — [PR #38](https://github.com/serhabdel/SeraBot/pull/38)
  (`b9c4135`).
- **Chat deepen (0.29)** — [PR #42](https://github.com/serhabdel/SeraBot/pull/42)
  (`d62e869`).
- **In flight** — `release/0.30.0` (B7 gate + version bump);
  `feat/wave-c3-presets` (C3 prompt blocks).

## Active workstreams (in progress)

- [x] **Wave A backend APIs** — A1–A3, A5–A7.
- [x] **Desktop Workflows tab** — A4.
- [~] **Chat UX polish** — PR #42: stable streaming merge, 60fps SSE pacing,
      composer focus/draft isolation, caret bridge (`ui_files.rs`). Remaining:
      optional smooth autoscroll / spring micro-motion (non-blocking).

---

## Wave A — See the machine (0.29.0)

### Server (`crates/serabot`)

- [x] **A1**–**A3**, **A5**–**A7** (see spec).

### Desktop (`crates/serabot-desktop`)

- [x] **A4** Workflows tab.

### Wave A gate

- [x] **Shipped** — PR #37 merged to `main` (0.29.0 Wave A).
- [ ] **A8** Phase gate: desktop smoke (Workflows tab, one trace, vault test);
      owner demo script. Automated gate green on `main` @ `d62e869` (2026-11-10).

---

## Wave B — Reliable circle (0.30.0)

- [x] **B1** Per-user routine CRUD — PR #39.
- [x] **B2** Merge watch into routines UX — PR #39.
- [x] **B3** Per-user source overlay (v1) — PR #41.
- [x] **B4** Reliability enqueue rules — PR #41.
- [x] **B5** Standing grants API + UI — PR #40.
- [x] **B6** Jobs panel stuck/unknown — PR #40.

### Wave B gate

- [~] **B7** Phase gate — Windows `cargo test --workspace --locked` green
      2026-11-10 on `release/0.30.0` worktree; release branch + `0.30.0` version
      bump next; isolation + URL policy review for B3 done in PR #41 tests.

---

## Wave C — Deep ops (0.31.0)

- [x] **C1** Browser console upgrades — PR #38.
- [ ] **C2** Vault accounts view — local + external refs + cards; domain filter;
      last used from audit.
- [ ] **C3** Autonomy presets — watcher/analyst prompt blocks reference
      Workflows + `watch_now` + `plan_update`; no new tools (`feat/wave-c3-presets`).
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
