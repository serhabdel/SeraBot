# Big Wave — task list

Spec: `docs/specs/big-wave.md` (owner-approved 2026-11-09). Prior release:
`docs/specs/release-0.28.0.md`, `tasks/release-0.28.0.md`. Gate per chunk:
`scripts\cargo-win.bat check --workspace --all-targets` +
`scripts\cargo-win.bat test --workspace --locked`. WSL fmt + clippy `-D warnings`
before each wave commit. Push after each wave gate; **never merge without the
owner's OK**.

## Post-merge tracker (2026-11-10)

- **`main` tip** — `bca2c1b` after [PR #53](https://github.com/serhabdel/SeraBot/pull/53)
  (desktop chat streaming scroll polish on #42); [PR #52](https://github.com/serhabdel/SeraBot/pull/52)
  (tracker refresh); [PR #51](https://github.com/serhabdel/SeraBot/pull/51) (A8 agent smoke);
  [PR #50](https://github.com/serhabdel/SeraBot/pull/50) (E1/E2); C5–C1 on `main`.
- **Wave A** — [PR #37](https://github.com/serhabdel/SeraBot/pull/37) (0.29.0).
- **Wave B** — [PR #39](https://github.com/serhabdel/SeraBot/pull/39)–#41;
  0.30.0 via [PR #43](https://github.com/serhabdel/SeraBot/pull/43).
- **Wave C** — C1 [PR #38](https://github.com/serhabdel/SeraBot/pull/38); C2 #45;
  C3 #44; C4 [PR #46](https://github.com/serhabdel/SeraBot/pull/46); C5 #48;
  **C6** 0.31.0 gate on `main`.
- **Chat deepen** — [PR #42](https://github.com/serhabdel/SeraBot/pull/42); streaming scroll
  polish [PR #53](https://github.com/serhabdel/SeraBot/pull/53) (`chat_scroll.rs`).

**Big Wave program** — Waves A–C, **E1**/**E2**, and chat deepen (#42, #53) are on `main`;
  **A8 owner smoke** (owner OIDC) remains (not a full program sign-off).

### Wrap-up gate evidence @ `bca2c1b` (2026-11-10)

Windows isolated target dirs (H: full; C: filled after check):

| Layer | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked` | **Pass** (`CARGO_TARGET_DIR=C:\cargo-target-serabot-wrapup`, ~4m14s) |
| `cargo test --workspace --locked` | **Pass** (`CARGO_TARGET_DIR=F:\cargo-target-serabot-wrapup`, ~7m49s) |

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

### Automated A8 pass (PR `feat/hardening-a8`, 2026-11-11)

The steps above are now also covered by an in-process test,
`crates/serabot/src/api/tests/a8_smoke_api.rs`, which runs in the normal
`cargo test --workspace` gate. It seeds an admin and a member through the store
and device tokens, then calls the same endpoints the console uses:

| Step | Automated check |
| --- | --- |
| 1 | Admin `GET /v1/routines`, `/v1/workflows/active`, `/v1/monitoring/sources` show the seeded routine, plan and failed source row. |
| 2 | Admin `GET /v1/jobs` lists the queued task and `reliability.traces_path`; `GET /v1/traces` returns a list. |
| 3 | Admin `GET /v1/logs?q=tool=` returns the `tool=` line, and the bearer-shaped value does not appear. |
| 4 | Admin `PUT` then `POST .../onepassword/test` returns exactly `ok`, `cli_found`, `configured`, `message`, and never the token. |
| 5 | Admin `GET /v1/admin/status` has uptime, process memory and storage categories; `GET /v1/usage` has the quota block. |
| 6 | Member sees only their own routine (`08:00`, not `07:30`), plan and queued tasks; admin source run status and error text are not shown. |
| 7 | Member `GET /v1/admin/status` and `GET /v1/logs` return 403; member `GET /v1/usage` has `scope: me` and no health card. |

Not covered: the live WebView2 render, the owner's Microsoft sign-in, and real
Credential Manager. The A8 owner smoke stays open and is not marked complete.

Desktop sign-in changes in the same PR (`docs/desktop-production-gaps.md`):
loopback `Host` check and ignored-request budget, configurable wait
(`sign_in_timeout_secs`, 60–900 s), copy-link fallback when the browser does
not open, and a credential-save warning with a retry button.

Demo script (spec two-week cut): routine countdown visible, one red source row,
one active plan, vault Test OK, one browser tool in traces without secrets.

### Agent automated pass (2026-11-10)

Checked out `main` @ `112a6e06` (PR #50). Did **not** mark A8 complete; owner
smoke + demo below still required.

| Layer | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked` | **Pass** (Windows) |
| `cargo test --workspace --locked` | **Pass** |
| `serabot-desktop` build + `--version` | **Pass** (`0.31.0`) |
| Wave A HTTP (`wave_a_visibility_api`, vault probe) | **Pass** |
| E1/E2 API (`memory_settings_api`, usage quota fields) | **Pass** |
| Desktop vault probe parsing (`serabot-desktop` tests) | **Pass** |
| Live WebView2 smoke (steps 1–7) | **Blocked** — owner session (Microsoft
  sign-in / Credential Manager token); no agent access to secrets; local
  `:8787` not listening; production origin responds but needs auth. |

**Extras (E1/E2, not in step table):** quota meter + Mem0 Settings UI verified
in source (`ui_usage.rs`, `ui_settings_page.rs`); not exercised against a live
server from this agent run.

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
