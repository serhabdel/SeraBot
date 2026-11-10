# Big Wave — task list

Spec: `docs/specs/big-wave.md` (owner-approved 2026-11-09). Prior release:
`docs/specs/release-0.28.0.md`, `tasks/release-0.28.0.md`. Gate per chunk:
`scripts\cargo-win.bat check --workspace --all-targets` +
`scripts\cargo-win.bat test --workspace --locked`. WSL fmt + clippy `-D warnings`
before each wave commit. Push after each wave gate; **never merge without the
owner's OK**.

## Post-merge tracker (2026-11-10)

- **`main` tip** — `b7e2c2e` after [PR #45](https://github.com/serhabdel/SeraBot/pull/45)
  (C2 vault accounts), [PR #44](https://github.com/serhabdel/SeraBot/pull/44)
  (C3 presets), [PR #43](https://github.com/serhabdel/SeraBot/pull/43) (0.30.0).
- **Wave A** — [PR #37](https://github.com/serhabdel/SeraBot/pull/37) (0.29.0).
- **Wave B** — [PR #39](https://github.com/serhabdel/SeraBot/pull/39),
  [PR #40](https://github.com/serhabdel/SeraBot/pull/40),
  [PR #41](https://github.com/serhabdel/SeraBot/pull/41); 0.30.0 via #43.
- **Wave C** — C1 [PR #38](https://github.com/serhabdel/SeraBot/pull/38);
  C2 #45; C3 #44; C4 [PR #46](https://github.com/serhabdel/SeraBot/pull/46) open.
- **Chat deepen** — [PR #42](https://github.com/serhabdel/SeraBot/pull/42).

---

## A8 — Desktop smoke checklist (owner)

Run against a local or staging server with two test accounts (admin + member).
Gate must be green before the smoke; record pass/fail, not screenshots of secrets.

| Step | Action | Pass criteria |
| --- | --- | --- |
| 1 | Admin → **Workflows** tab, **Refresh** | Routines countdown, source health rows, and active plans load without opening chat. |
| 2 | Admin → **Jobs** | Uncertain / outcome-unknown rows appear when present; trace link opens **Logs** or traces path. |
| 3 | Admin → **Logs** | Search `q=tool=` shows a redacted `tool.completed` line (no canonical secret args). |
| 4 | Admin → **Vault** | **Test** on a provider returns `{ok, cli_found, configured, message}` only; missing CLI fails gracefully. |
| 5 | Admin → **Usage** | Server health card loads (uptime, memory, storage categories); counts only, no member message text. |
| 6 | Member → **Workflows** | Member sees **only** their routines/sources/plans; admin rows are not visible. |
| 7 | Member → **Usage** | No server health card (403 backstop if forced via API). |

Demo script (spec two-week cut): routine countdown visible, one red source row,
one active plan, vault Test OK, one browser tool in traces without secrets.

---

## Wave A — See the machine (0.29.0)

- [x] **A1**–**A7**, **A4** desktop Workflows tab — PR #37.
- [ ] **A8** Phase gate — automated gate green on `main`; desktop smoke table
      above + owner demo still outstanding.

---

## Wave B — Reliable circle (0.30.0)

- [x] **B1**–**B6** — PRs #39–#41.
- [x] **B7** — 0.30.0 release merged (#43).

---

## Wave C — Deep ops (0.31.0)

- [x] **C1** Browser console upgrades — PR #38.
- [x] **C2** Vault accounts view — PR #45.
- [x] **C3** Autonomy presets — PR #44.
- [ ] **C4** Admin reliability dashboard — PR #46 (scheduler/source counts on
      Usage server card).
- [ ] **C5** Flutter Workflows + traces — read-only parity with desktop.
- [ ] **C6** Phase gate; `flutter analyze` / `flutter test` when C5 lands.

---

## Cross-wave extras (from spec theme 6)

- [ ] **E1** Quota near-limit surfacing in console — `GET /v1/usage` exists;
      UI tile still TODO (near-limit thresholds in config / store quotas).
- [ ] **E2** Mem0 provider — documented in `docs/memory.md`; optional Settings
      toggle when `[memory] provider = "mem0"` still TODO.
- [x] **E3** `docs/README.md` index row for Big Wave spec (this pass).

---

## Two-week cut tracker (Wave A only)

| Week | Target |
| --- | --- |
| **1** | A1, A2, A3, A6 + tests; spec/tasks in repo (this file). |
| **2** | A4, A5, A7 + Wave A gate + owner demo. |
