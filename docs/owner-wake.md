# Owner wake checklist (synthetic)

Morning pass after the overnight work (2026-10-29). Examples only — never paste
real API keys, tokens, host passwords, age-store contents, or login URLs into
chat, tickets, or git.

## What's on `main`

| Item | Value |
| --- | --- |
| **OVH code tip** | `23fcb7d` — *Keep room turns on the orchestrator and document token rotation.* |
| **Previous host tip** | `dc4d2e4` — *List configured bots and label room handoff replies.* |
| **Pre-redaction host tip** | `4464558` — *Redact Telegram bot tokens before they reach the journal.* The last tip that ran before token redaction landed; see the rotation note below. |
| **Routing decision** | `bd871f9` — *Record why routing stays on the in-process allowlist.* Docs only (`docs/model-routing.md`). |
| **This checklist** | Documentation on top of `23fcb7d`. It does not move the binary. |

`23fcb7d` (`23fcb7d85ec75dbf3e5f10912c4e974b79a7aed6`) is the binary on OVH.
`dc4d2e4` (`dc4d2e4a49cc47ad920dcbe7d484ab0892c99623`) was the previous host
tip, and `4464558` (`4464558aa9ab8dd78666430760f3cc6d3c725c5c`) the one before
that. Docs-only commits after `23fcb7d`, including this page, do not need
another deploy.

### Token rotation if journals held `getUpdates` URLs

`4464558` drops the Bot API request URL and redacts `bot<token>` segments
before an error is logged. Builds **before** it could write a `getUpdates`
URL — which embeds the bot token — into the journal. The `4464558` binary ran
until `dc4d2e4` replaced it, so anything logged before that swap is in scope.

If any historical journal line contains a `getUpdates` URL:

1. Rotate `telegram_bot_token` in **BotFather** (`/revoke` on that bot).
2. Store the new value with `serabot secrets set telegram_bot_token`, reading
   stdin. Never pass it as an argument and never paste it into chat, tickets,
   or git.
3. Restart **only** `serabot.service`.

Do not paste the token, the journal line, or the URL anywhere. If unsure
whether a journal line existed, rotate anyway; the cost is one BotFather step.
This note is the same rule recorded under [OVH](#ovh) below.

Landed on `main` since `4464558` (code through `23fcb7d`):

- `GET /v1/bots` lists configured bots (`id`, `role`, `enabled`, `live`, tools, skills) without prompts or secrets. Desktop Fleet has **Refresh bots**.
- Room turns label `main`, and a `delegate` handoff keeps the specialist id on that same conversation. `GET /v1/conversations/{id}/messages` returns `name` when it is a bot id. A `bot` override on a `room:` id is ignored, so only `main` starts work there (`23fcb7d`) ([orchestration.md](orchestration.md#rooms-and-delegate)).
- Tag pushes (`v*`) build and publish client downloads on GitHub Releases: Windows desktop exe + zip and Android APK/AAB (`.github/workflows/release.yml`). CI-only; no OVH deploy ([ci-and-secrets.md](ci-and-secrets.md#releases)).

Still on `main` since `795b139` (code through `4464558`):

- Telegram request URLs are dropped, and `bot<token>` segments are redacted, before an error is logged (`4464558`).
- `skill_load` returns workflow text, and client builds can take a server origin (`4b1f0a4`).

Still on `main` from the earlier wake note (code through `795b139`):

- On-demand skills and `skill-creator`, including `POST /v1/skills` ([skills.md](skills.md)).
- Crafting Apps notes: adapt PdfCraft patterns only ([craft-adoption.md](craft-adoption.md)).
- Orchestrator handoffs, delegate, and the conversation tree ([orchestration.md](orchestration.md)).
- Owner APIs for bots, conversations, and skills ([api.md](api.md)).
- Desktop and mobile Fleet panels ([desktop-client.md](desktop-client.md), [mobile-client.md](mobile-client.md)).
- Native provider kinds and fail-closed catalogs, with Mistral, DeepSeek, and MiniMax host-pinned ([model-routing.md](model-routing.md)).
- Antigravity loopback PKCE login and Cloud Code chat ([antigravity.md](antigravity.md)).
- Loopback browser CDP sessions, off by default ([browser.md](browser.md)).
- Configure-later Google, Outlook, and Notion OAuth stubs, plus native AgentMail ([connectors.md](connectors.md)).
- Rotated OAuth refresh tokens (OneDrive, connectors, Antigravity) written back to the age store. A failed write fails the call.
- Public CI rules: workflows do not read owner tokens ([ci-and-secrets.md](ci-and-secrets.md)).
- Routing decision in `bd871f9`: keep the in-process allowlist `ModelRouter`. LiteLLM, RouteLLM, and OpenRouter auto-routing stay out.

## OVH

Confirmed on code tip **`23fcb7d`**
(`23fcb7d85ec75dbf3e5f10912c4e974b79a7aed6`). `serabot` is active. Binary
SHA-256 `b0cc4ca9a8dddd0f79aba44fba4c8a7740d35cb293c79b7a3239a3f354620738`.
Staged at `/var/lib/serabot/releases/serabot-23fcb7d`. The previous binary is
`/var/lib/serabot/releases/serabot-pre-23fcb7d` (code tip `dc4d2e4`, SHA-256
`71f3c5fd8c6fbe549e49f1f67610aed7338a88caf19001261897472bf00a1e8d`).
Built on the host from `23fcb7d` with `cargo build --release --locked -p serabot`
on Ubuntu 24.04.5 (glibc 2.39). `onedrive-mcp` was left untouched and is still
active. Unauthenticated `GET /v1/health` and `GET /v1/bots` return 401.
`/v1/health` reports crate version `0.1.0`, not a git SHA.

If historical journals ever contained `getUpdates` URLs (a build before
`4464558`), rotate `telegram_bot_token` in BotFather and store the new value
with `serabot secrets set`. Do not paste the token, the journal line, or the
URL into chat, tickets, or git.

This page does not change the binary. Roll back by restoring
`serabot-pre-23fcb7d` onto `/opt/serabot/serabot` and restarting **only**
`serabot.service`. Leave Docker, OpenBot, `onedrive-mcp.service`, and the
Cloudflare tunnel alone. Before replacing a busy binary, tunnel in and read
`GET /v1/status`. Queued turns without a pinned route fail closed. The queue
was empty of non-terminal tasks at this deploy (15 done and 2 failed rows only).

If historical journals ever contained `getUpdates` URLs (a build before
`4464558`), rotate `telegram_bot_token` in BotFather and store the new value
with `serabot secrets set`. Do not paste the token, the journal line, or the
URL into chat, tickets, or git.

This page does not change the binary. Roll back by restoring
`serabot-pre-23fcb7d` onto `/opt/serabot/serabot` and restarting **only**
`serabot.service`. Leave Docker, OpenBot, `onedrive-mcp.service`, and the
Cloudflare tunnel alone. Before replacing a busy binary, tunnel in and read
`GET /v1/status`. Queued turns without a pinned route fail closed. The queue
was empty of non-terminal tasks at this deploy (done and failed rows only).

The earlier read-only check (mtime `2026-10-29 01:39:08` UTC, SHA-256 prefix
`dfbc71dd89536f4e…`) and the `795b139` binary both predate `4464558`.

New bots and skills are **not** hot-reloaded. `POST /v1/bots` and
`POST /v1/skills` report `live: false`. `GET /v1/bots` lists the files anyway,
with `live: false` until restart. Restart **only** `serabot.service` after
those files exist, then add the skill id to a bot allowlist. A scaffolded bot
stays `enabled = false` and is not added to `main.delegates` until you edit
that and restart again.

```bash
systemctl status serabot --no-pager
journalctl -u serabot -n 50 --no-pager
```

Build the release on Ubuntu 24.04 glibc, not a newer WSL userland ([deployment.md](deployment.md)).

## Configure later

Do these when you want the feature. Leave the sections disabled until the named
secrets exist. `serabot secrets set <name>` reads stdin and prints nothing.
Commands below use secret **names** only.

| Item | When you are ready |
| --- | --- |
| **Google** (Drive, Sheets, Calendar, Gmail) | Create the Cloud OAuth client and a loopback redirect. `secrets set` `google_oauth_client_id`, `google_oauth_client_secret`, and the product refresh token (`google_drive_refresh_token`, `google_sheets_refresh_token`, `google_calendar_refresh_token`, `gmail_refresh_token`). Then `enabled = true` on that `[connectors.*]` section only. |
| **Outlook** | Entra app with `Mail.Read` or `Calendars.Read` plus `offline_access`. Names: `outlook_client_id`, `outlook_client_secret`, `outlook_refresh_token`, `outlook_calendar_refresh_token`. |
| **Notion** | Public integration. Names: `notion_oauth_client_id`, `notion_oauth_client_secret`, `notion_refresh_token`. |
| **AgentMail** | `secrets set agentmail_api_key`. Set `default_inbox_id` or pass `inbox_id` (letters, digits, `_`, `-`). Enable `[connectors.agentmail]` only after the key is stored. |
| **Enable a connector** | One section at a time, after its keys exist. Add the tool to a bot allowlist. Reads and AgentMail sends stay approval-gated. Restart **only** `serabot.service`. |
| **Antigravity** | `secrets set antigravity_oauth_client_secret`, point `[llm.providers.*]` at `kind = "google-antigravity"`, then `serabot providers login google-antigravity`. Run that on the machine whose browser can open `127.0.0.1:51121`. Set `data_egress_approved = true` only when you intend Cloud Code to receive prompts. See [antigravity.md](antigravity.md). |
| **GitHub Actions client builds** | `.github/workflows/clients.yml` accepts optional secrets `SERABOT_OWNER_API_URL` (synthetic shape `https://owner.example`, baked as the client default) and documents `SERABOT_OWNER_TOKEN`. The workflow does **not** read the token secret and does not compile a token. Unset secrets leave CI green. Never commit secret values ([ci-and-secrets.md](ci-and-secrets.md)). |
| **GitHub Releases (client downloads)** | `git tag v0.1.0 && git push origin v0.1.0` runs `.github/workflows/release.yml`: builds `serabot-desktop` (Windows exe + zip) and `apps/serabot_mobile` (APK + AAB) and posts them to the Releases page. Reads only the optional `SERABOT_OWNER_API_URL` secret; the token is never read or compiled. APK/AAB are debug-signed (sideload only) until a keystore secret exists. See [ci-and-secrets.md](ci-and-secrets.md#releases). |
| **Hot reload** | Not implemented. Restart `serabot.service` for new bots and skills (OVH section above). |

Still owner-gated from the previous checklist. Same stdin rule.

| Secret / flag | When needed |
| --- | --- |
| `firecrawl_api_key` + `[firecrawl] enabled = true` | Firecrawl tools ([firecrawl.md](firecrawl.md)) |
| `elevenlabs_api_key` + `[elevenlabs] enabled = true` | ElevenLabs tools ([elevenlabs.md](elevenlabs.md)). The key may already be in the age store — confirm the section in `/etc/serabot/serabot.toml`. |
| `[audio] enabled`, `telegram_intake` | Owner voice notes in Telegram ([audio.md](audio.md)) |
| `onedrive_client_id`, `onedrive_refresh_token` | Only if rotating. Export the refresh token from the MCP MSAL cache per [deployment.md](deployment.md). Production already had `[onedrive] enabled = true`, `tenant = "consumers"`, on the last read-only check. |
| `[browser] enabled` | Leave `false` until you want a short loopback CDP session ([browser.md](browser.md)). |

Repo defaults keep `[connectors.*]` and `[browser]` at `enabled = false`.

## Smoke commands

Put the owner token in your shell as `TOKEN`. Do not echo it.

```bat
ssh -L 8787:127.0.0.1:8787 ovh-vps
```

Read-only, from the machine that holds the tunnel:

```bash
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8787/v1/health
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8787/v1/status
curl -sS -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:8787/v1/models?conversation_id=owner"
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8787/v1/skills
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8787/v1/bots
```

Health should show `ok` true, `version` `0.1.0`, and the database up. Status
is pending tasks, pending approvals, and the loopback API. Models lists
allowlisted pairs and must not include endpoints or credentials. Skills
returns on-disk summaries, not bodies. Bots returns `id`, `role`, `enabled`,
`live`, tools, and skills — not prompts or secrets.

Telegram, about five minutes. The service is on `23fcb7d`:

1. Owner chat — command menu populated (`setMyCommands` on poll startup).
2. **`/providers`** — models grouped by provider. Catalogs fail closed when refresh fails.
3. **`/model auto`**, then **`/model <provider> <model>`** — selection pins for this conversation ([model-routing.md](model-routing.md)).
4. **`/help`** — `/brief`, `/watch`, schedules, approvals, deck flows.
5. Firecrawl, audio, and ElevenLabs only after the secret rows above. `elevenlabs_list_voices` is the read-only probe; generation stays Once / Always / Deny.

Optional writes create files on the host the API is using. Skip them on OVH
unless you want those files there. Ids are synthetic. Bot and skill
responses report `live: false`. Sections do not accept turns.

```bash
curl -sS -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"id":"notes","description":"Synthetic notes specialist.","tools":["skill_list"],"skills":["ask"]}' \
  http://127.0.0.1:8787/v1/bots

curl -sS -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"id":"notes-pack","description":"Synthetic notes workflow.","body":"# Notes\n\nUse allowlisted tools.\n"}' \
  http://127.0.0.1:8787/v1/skills

curl -sS -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"kind":"section","slug":"finance"}' \
  http://127.0.0.1:8787/v1/conversations
```

Prebuilt Windows and Android clients download from GitHub Releases (tag `v*`);
local launch steps are unchanged: [desktop-client.md](desktop-client.md),
[mobile-client.md](mobile-client.md).
Desktop Fleet refreshes skills with `GET /v1/skills` and bots with
`GET /v1/bots`. Mobile Fleet still submits the three POST endpoints. Local
check before the next OVH build:

```bat
scripts\cargo-win.bat check --workspace --all-targets
scripts\cargo-win.bat test --workspace --locked
scripts\cargo-win.bat test -p serabot-desktop --locked
```

OneDrive after the maker bot is up: **`onedrive_list`** (read), then
**`onedrive_upload`** with approval. Keep `onedrive-mcp.service` running.

## Not verified live

Unit tests use synthetic fixtures. Treat Telegram, Firecrawl, ElevenLabs,
voice intake, OneDrive uploads, Antigravity, connector OAuth, AgentMail, and
browser CDP as owner-verified only after you exercise them on the real
account. OVH is confirmed on code tip `23fcb7d`; that confirmation is the
binary and the unauthenticated health check, not a live exercise of those
tools. If journals from before `4464558` contain a `getUpdates` URL, rotate
`telegram_bot_token` as described above. Do not paste the token.
