# Owner wake checklist (synthetic)

Morning pass after the overnight work. Examples only — never paste real API keys,
tokens, or MSAL cache contents into chat, tickets, or git.

## Git and OVH binary (evidence, not secrets)

| Item | Value |
| --- | --- |
| **Git `main` HEAD** | `6d718fe` — *Allowlist ElevenLabs tools on main and maker bots.* |
| **Remote** | `origin/main` matches local `main` (clean tree at doc time) |
| **OVH `/opt/serabot/serabot` mtime (UTC)** | `2026-10-29 01:39:08` |
| **OVH binary SHA-256 (prefix)** | `dfbc71dd89536f4e…` (full hash from `sha256sum /opt/serabot/serabot` on host) |
| **Mapped git SHA on binary** | **Not stamped** — compare SHA after each on-host Ubuntu 24.04 build per [deployment.md](deployment.md). Prior session noted deploy around `3504eb9`; the `01:39` UTC binary may be newer. |

`serabot.service` was **active** on read-only SSH check. `/var/lib/serabot/releases` listing requires the `serabot` user (permission denied as login user).

## Telegram (5 minutes)

1. Open the owner chat — confirm the **command menu** is populated (bot calls `setMyCommands` on poll startup).
2. **`/providers`** — models grouped by provider (same allowlist as flat **`/models`**).
3. **`/model auto`** then **`/model <provider> <model>`** — confirm selection pins for this conversation ([model-routing.md](model-routing.md)).
4. **`/help`** — lists `/brief`, `/watch`, schedules note, approvals, deck flows.
5. **Firecrawl** — only after secrets + config below; trigger a small **`firecrawl_scrape`** and confirm approval UI (destination + policy bind).
6. **Audio** — send a **voice note** only if `[audio] enabled = true` and `telegram_intake = true` on production; otherwise use inbox + `audio_transcribe` / `audio_understand` ([audio.md](audio.md)).
7. **ElevenLabs** — try **`elevenlabs_list_voices`** (read-only), then a generation tool (e.g. TTS) and confirm **Once / Always / Deny** approval ([elevenlabs.md](elevenlabs.md)).

Scheduler: daily routines run from SQLite via `due_routines` in the daemon loop — not separate Telegram commands. On-demand **`/brief`** and **`/watch`** are documented in **`/help`**.

## Windows desktop (egui)

From repo root on Windows (MSVC):

```bat
scripts\cargo-win.bat test -p serabot-desktop --locked
scripts\cargo-win.bat run -p serabot-desktop --locked
```

Tunnel to OVH Owner API (example):

```bat
ssh -L 8787:127.0.0.1:8787 ovh-vps
```

Connect with loopback URL `http://127.0.0.1:8787`, owner API token, conversation id. See [desktop-client.md](desktop-client.md).

## Flutter mobile (Android MVP)

```bat
cd apps\serabot_mobile
flutter pub get
flutter test
flutter run
```

Release APK: `flutter build apk --release` → `build/app/outputs/flutter-apk/`.  
Emulator to host: `adb reverse tcp:8787 tcp:8787` then `http://127.0.0.1:8787`.  
Details: [apps/serabot_mobile/README.md](../apps/serabot_mobile/README.md).

## OneDrive status

| Layer | Status |
| --- | --- |
| **Repo default** (`serabot.toml`) | `[onedrive] enabled = false` until secrets exist |
| **OVH production** (read-only check) | `[onedrive] enabled = true`, `tenant = "consumers"` |
| **MCP on host** | Keep `onedrive-mcp.service` running — do not reconfigure; native Graph reuses the same public-client + refresh-token pattern ([deployment.md](deployment.md), `crates/serabot/src/onedrive.rs`) |

Verify after maker bot is available: **`onedrive_list`** (read), then **`onedrive_upload`** with approval.

## Remaining secret / config steps (owner)

Do on OVH as `serabot` user with production config — **stdin only**, never log values.

| Secret / flag | When needed |
| --- | --- |
| `firecrawl_api_key` + `[firecrawl] enabled = true` | Firecrawl tools ([firecrawl.md](firecrawl.md)) |
| `elevenlabs_api_key` + `[elevenlabs] enabled = true` | ElevenLabs tools ([elevenlabs.md](elevenlabs.md)); key may already be in age store — confirm section enabled in `/etc/serabot/serabot.toml` |
| `[audio] enabled`, `telegram_intake` | Owner voice notes in Telegram ([audio.md](audio.md)) |
| `onedrive_client_id`, `onedrive_refresh_token` | Only if rotating; export refresh from MCP MSAL cache per [deployment.md](deployment.md) |

After any production config change: restart **only** `serabot.service`, then `journalctl -u serabot -n 50`.

## Local verification (before the next deploy)

```bat
scripts\cargo-win.bat check --workspace --all-targets
scripts\cargo-win.bat test --workspace --locked
```

Linux release builds for OVH must use **Ubuntu 24.04** glibc — not WSL 26.04 native artifacts ([deployment.md](deployment.md)).

## Not verified live (honest boundary)

Wiremock/unit tests cover Firecrawl, ElevenLabs, OneDrive, and desktop HTTP client. Treat Telegram menus, voice intake, Firecrawl, ElevenLabs generations, and OneDrive uploads as **owner-verified** only after you exercise them on the real account.
