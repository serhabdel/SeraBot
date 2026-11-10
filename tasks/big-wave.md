# Big Wave — task list

Spec: `docs/specs/big-wave.md` (owner-approved 2026-11-09). Prior release:
`docs/specs/release-0.28.0.md`, `tasks/release-0.28.0.md`. Gate per chunk:
`scripts\cargo-win.bat check --workspace --all-targets` +
`scripts\cargo-win.bat test --workspace --locked`. WSL fmt + clippy `-D warnings`
before each wave commit. Push after each wave gate; **never merge without the
owner's OK**.

## Post-merge tracker (2026-11-10)

- **`main` tip** — `f55e941` after [PR #48](https://github.com/serhabdel/SeraBot/pull/48)
  (C5 Flutter Workflows/traces); [PR #47](https://github.com/serhabdel/SeraBot/pull/47)
  (tracker/docs); C4–C1 chain on `main`.
- **Wave A** — [PR #37](https://github.com/serhabdel/SeraBot/pull/37) (0.29.0).
- **Wave B** — [PR #39](https://github.com/serhabdel/SeraBot/pull/39)–#41;
  0.30.0 via [PR #43](https://github.com/serhabdel/SeraBot/pull/43).
- **Wave C** — C1 [PR #38](https://github.com/serhabdel/SeraBot/pull/38); C2 #45;
  C3 #44; C4 [PR #46](https://github.com/serhabdel/SeraBot/pull/46); C5 #48;
  **C6** 0.31.0 gate (this branch).
- **Chat deepen** — [PR #42](https://github.com/serhabdel/SeraBot/pull/42).

**Big Wave program** — Waves A–C features are on `main` after C6; **A8 smoke**,
**E1**, and **E2** remain (not a full program sign-off).

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

## C6 — Wave C gate checklist (0.31.0)

Automated expectations before merge; owner OK required.

| Layer | Command / CI | Notes |
| --- | --- | --- |
| Rust workspace | `cargo check --workspace --all-targets`, `cargo test --workspace --locked` | `scripts\cargo-win.bat` on Windows; WSL fmt + clippy `-D warnings` before commit. |
| Desktop client | `clients.yml` → `desktop` job | `cargo check/test -p serabot-desktop` on Ubuntu + Windows; Windows release exe artifact. |
| Flutter (C5) | `clients.yml` → `mobile` job | `flutter analyze`, `flutter test`, debug APK upload, **release APK compile check** on PR/`main` — no local Android SDK required (`docs/mobile-client.md`). |

Release notes: `docs/release-0.31.0.md`.

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
- [x] **C4** Admin reliability dashboard — PR #46.
- [x] **C5** Flutter Workflows + traces — PR #48.
- [x] **C6** Phase gate — workspace 0.31.0; Rust + `clients.yml` Flutter gate
      (see checklist above).

---

## Cross-wave extras (from spec theme 6)

- [x] **E1** Quota near-limit surfacing in console — `quota.daily_tasks` on
      `GET /v1/usage`; Usage page meter + near/at-limit warning.
- [x] **E2** Mem0 provider — `docs/memory.md` console notes; Settings → Memory
      when `[memory] provider = "mem0"` (`GET/PUT/DELETE /v1/memory/mem0/key`).
- [x] **E3** `docs/README.md` index row for Big Wave spec (PR #47).

---

## Two-week cut tracker (Wave A only)

| Week | Target |
| --- | --- |
| **1** | A1, A2, A3, A6 + tests; spec/tasks in repo (this file). |
| **2** | A4, A5, A7 + Wave A gate + owner demo. |
