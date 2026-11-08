# Vault (0.23)

The vault holds the logins and API keys a bot may use for you. A bot never
sees a value. It names an entry, and the server types the value into a page
after you approve it for that site.

One vault per user, shared by that user's bots. Every row is scoped to the
owner. Nothing here is readable by another member or by the admin.

## Sources

Each entry has a source (`source` in the API, `local` by default):

| Source | Where the value lives | What the entry stores |
| --- | --- | --- |
| `local` | Sealed in `user_credentials` (per-user envelope encryption) | The value. |
| `onepassword` | Your 1Password account | `reference`: `op://<vault>/<item>/<field>` (or with a section). |
| `protonpass` | Your Proton Pass account | `reference`: `pass://<vault>/<item title>/<field>`. |

A reference holds a pointer, never a value. The value is read when the
browser types it, through your own password manager CLI, and is not kept.

Metadata (name, kind, username, sites, note, reference, timestamps) is stored
in `vault_entries`. Migration `0028_vault_providers.sql` added `source` and
`reference`; existing rows are `local`.

## Entry rules

- Name: `^[a-z0-9][a-z0-9-]{0,47}$`.
- Kinds: `password` (username and password), `api_key`, `note`. A note cannot
  come from a password manager and is never typed into a page.
- **Sites are required for a fill.** An entry with no sites is never typed
  anywhere. Sites are host names (`example.com` also covers its subdomains).
  A reference imported from a login with no URL has no sites until you add one.
- At most 64 entries per user.

## Secret request card

A bot that needs a login it does not have calls `vault_request_secret`
(name, kind, sites, username, reason, and `purpose`):

- `purpose: save` stores a new login. `purpose: provide` uses an existing
  entry. If the entry exists, the tool answers `exists` and asks for nothing.
- The tool raises a **secret request card** in the chat. The card shows the
  name, kind, username, sites and the reason. You enter the value in a masked
  field and press Save. The value goes to `POST /v1/cards/{id}/secret`.
- The server validates the value (1 to 8192 characters, no control
  characters), seals it, marks the card answered and posts a message to the
  conversation: `Secret “<name>” was saved to the vault for <sites>. Use it
  with browser_type secret=<name>.` The message and the card response never
  contain the value.
- The route reads at most 16 KiB. A card resolves once. A `save` card
  replaces an existing entry of that name. A `provide` card never overwrites
  one (409).
- At most one secret request is pending per conversation. Telegram chats have
  no card view, so the tool refuses there.
- Autonomy (yolo or a goal) does not skip a secret request. A secret always
  needs you.
- Decline is available on the card. The bot is told and continues without it.

## Using a secret: approval rules

`browser_type` with `secret=<name>` and `field` (`password`, `username` or
`value`) is the only way a bot uses a value.

1. The entry must allow the **current page's host**. An empty site list
   allows nothing. A refused host fails before any approval is asked.
2. Typing a secret **always needs your approval**, even in autonomy mode. The
   approval binds:
   - your user id and the entry name,
   - the canonical arguments (ref, secret, field, submit),
   - the destination `vault:<name> → <host>`,
   - the policy version and the expiry, as for every approval.
3. Yolo (the conversation's auto-approve) may approve a secret typing only
   when the page's host matches the entry's sites. Otherwise it stays pending.
4. The gateway re-reads the page's host just before dispatch. If it no longer
   matches the approved destination, the action is invalidated and nothing is
   typed. A new approval is needed.
5. A "remember" grant matches only the same entry, host and arguments.

The page host is read from the bot's own session. It is not taken from the
model.

## Password managers

Set the access token once in the desktop Vault, under **Password managers**.
The server seals it per user under `vaultprov:<id>`. It never returns it, and
it is never listed as an AI provider.

| Manager | Token variable (set for the CLI only) | CLI | Install |
| --- | --- | --- | --- |
| 1Password | `OP_SERVICE_ACCOUNT_TOKEN` | `op` | <https://developer.1password.com/docs/cli/get-started/> |
| Proton Pass | `PROTON_PASS_PERSONAL_ACCESS_TOKEN` | `pass-cli` | <https://protonpass.github.io/pass-cli/> |

The server finds the CLI on `PATH`, or at the path set in config:

```toml
[vault]
op_path = ""          # empty: look up `op` on PATH
pass_cli_path = ""    # empty: look up `pass-cli` on PATH
```

`GET /v1/vault/providers` reports, for each manager, whether a token is
saved (`configured`) and whether its CLI is installed (`cli_found`). The
token is written with `PUT /v1/vault/providers/{id}` `{"token": "..."}` and
removed with `DELETE`.

Reading a value runs the CLI with an argument array (never a shell), the
token only in the child's environment, a 15 second timeout (the process is
killed), and at most 64 KiB of output. Only the exit code is logged. Stderr is
discarded. The read is read-only: `op read --no-newline <ref>` for 1Password.

The Proton Pass arguments are `item view --vault-name <v> --item-title <i>
--field <f>`. **These flags are not verified against every CLI release.** The
shape lives in one function, `vault_providers::read_args`. Check it against
your installed `pass-cli` before relying on Proton Pass.

## Importing from this PC

The desktop can list your logins with the CLI on **this PC** (not the server),
using the same no-shell, 20 second, stdout-only rules:

- 1Password: `op item list --categories Login --format json`.
- Proton Pass: `pass-cli item list --output json`. The listing shape is
  assumed (`{"items": [...]}` or a bare array, with `title`, `vault_name` or
  `vault.name`, `username`, `urls`). Confirm it against your CLI.

You pick the logins in a picker that shows titles, usernames and sites. Each
pick becomes a **reference entry**: `op://<vault id>/<item id>/password` for
1Password, `pass://<vault>/<title>/password` for Proton Pass, the username as
metadata, and the site from the first URL. No password is read or stored
during the import. Names are made unique automatically.

If the CLI is not on the PC, the picker says so and links the install page.

## API

| Method | Route | Notes |
| --- | --- | --- |
| `GET` | `/v1/vault` | Entries (metadata only). Each has `source` and `reference`. |
| `PUT` | `/v1/vault/{name}` | `{kind, value, domains, username?, note?, source?, reference?}`. A reference takes `value: ""`. |
| `DELETE` | `/v1/vault/{name}` | |
| `GET` | `/v1/vault/providers` | Password manager status. No token. |
| `PUT` | `/v1/vault/providers/{id}` | `{"token": "..."}`. Write-only. |
| `DELETE` | `/v1/vault/providers/{id}` | |
| `POST` | `/v1/cards/{id}/secret` | `{"value": "..."}` for a secret request card. Body ≤ 16 KiB. |

## Bot tools

- `vault_list`: names, kinds, sources, sites and usernames. Never a value.
- `vault_request_secret`: raises a secret request card (non-destructive: no
  approval for the request itself, the owner enters the value in the card).
- `browser_type` with `secret`: approval per site, as above.

## Security notes

- The model never receives a value. Tool output is redacted (`[vault:<name>]`),
  and the browser lease scrubs values it typed from later page text.
- Provider-read values are not in the model redactor. They exist only in the
  browser lease, for the duration of one type.
- Audit rows carry the entry name and host, never a value. `vault.saved_via_card`
  records the name and card id.
- Secret cards are owner-scoped: another account gets 404 on the card.
- Secret request bodies are read with a 16 KiB limit. The HTTP layer's own
  buffer is not zeroed, so a value passes through transient memory once.

## Known limits

- The Proton Pass CLI flags and the Proton Pass listing shape are assumptions
  until checked against the installed CLI.
- A pending secret request does not expire. The owner can decline it.
- A reference is typed only by a bot that holds `browser_type`. Telegram
  chats cannot receive a secret card.
- No live account was used for testing. Every test uses synthetic fixtures and
  a fake CLI.
