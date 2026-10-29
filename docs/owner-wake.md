# Owner wake checklist (synthetic)

Morning pass after the overnight work (2026-10-29). Examples only — never paste
real API keys, tokens, host passwords, age-store contents, or login URLs into
chat, tickets, or git.

## What's on `main`

| Item | Value |
| --- | --- |
| **Code tip for the binary** | `795b139` — *Persist OneDrive refresh rotation and the first connector refresh token.* |
| **Routing decision** | `bd871f9` — *Record why routing stays on the in-process allowlist.* Docs only (`docs/model-routing.md`). |
| **This checklist** | Documentation on top of `bd871f9` and `cd62044`. It does not move the binary. |
| **Previous wake note** | `6d718fe` (ElevenLabs allowlist). Superseded by this page. |

Commits after `795b139`, including docs-only `bd871f9` and `cd62044`, plus this
page, do not change the server binary. A host already running `795b139` does
not need another deploy for them.

Landed on `main` since `6d718fe` (code through `795b139`):

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

Confirmed on code tip **`795b139`**
(`795b13937206fca7e379b3024008e1a1df182c52`). `serabot` is active. Binary
SHA-256 `795becec49bf5c178bc2c68f209b85471142516f2c78d0b9c8b2c6800f29f1e7`.
`onedrive-mcp` was left untouched. Unauthenticated `GET /v1/health` returns
401. `/v1/health` reports crate version `0.1.0`, not a git SHA.

Docs-only `bd871f9` and `cd62044` do not change that binary and do not need a
redeploy. The earlier read-only check (mtime `2026-10-29 01:39:08` UTC,
SHA-256 prefix `dfbc71dd89536f4e…`) predates this binary.

If a later `sha256sum /opt/serabot/serabot` disagrees, finish the additive
upgrade in [deployment.md](deployment.md): stage
`/var/lib/serabot/releases/serabot-795b139`, back up the current binary,
replace `/opt/serabot/serabot` only after the staged SHA-256 matches, and
restart **only** `serabot.service`. Leave Docker, OpenBot,
`onedrive-mcp.service`, and the Cloudflare tunnel alone. Roll back by
restoring the backup and restarting only `serabot.service`. Before replacing
a busy binary, tunnel in and read `GET /v1/status`. Queued turns without a
pinned route fail closed.

New bots and skills are **not** hot-reloaded. `POST /v1/bots` and
`POST /v1/skills` report `live: false`. Restart **only** `serabot.service`
after those files exist, then add the skill id to a bot allowlist. A
scaffolded bot stays `enabled = false` and is not added to `main.delegates`
until you edit that and restart again.

```bash
systemctl status serabot --no-pager
journalctl -u serabot -n 50 --no-pager
```

If `journalctl` shows a Telegram `getUpdates` URL from a build before token
redaction, rotate that bot token in BotFather and `secrets set` it under the
configured secret name (default `telegram_bot_token`). Do not copy the journal
line into chat, tickets, or git.

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
```

Health should show `ok` true, `version` `0.1.0`, and the database up. Status
is pending tasks, pending approvals, and the loopback API. Models lists
allowlisted pairs and must not include endpoints or credentials. Skills
returns on-disk summaries, not bodies.

Telegram, about five minutes. The service is on `795b139`:

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

Windows desktop and Flutter mobile launch steps are unchanged:
[desktop-client.md](desktop-client.md), [mobile-client.md](mobile-client.md).
Fleet on either client calls the same three endpoints. Local check before the
next OVH build:

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
account. OVH is confirmed on code tip `795b139`; that confirmation is the
binary and the unauthenticated health check, not a live exercise of those
tools.
