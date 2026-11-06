# Contract: browser page, browser profiles, bot settings, Vault page (0.16)

Status: in progress (0.16.0). Shared by the server and desktop work.

## Browser profiles (server)

A profile is a named, per-user Chrome user-data directory (its own cookies,
logins, local storage). Every user has a `default` profile that cannot be
deleted. Directory: `<browser base>/<user_id>/<profile_id>`.

- `GET /v1/browser/profiles` → `{"profiles":[{"id","name","is_default",
  "bots":[bot ids assigned],"created_at","last_used_at"?}]}`
- `POST /v1/browser/profiles` `{"name"}` → profile (id generated from name,
  `^[a-z0-9][a-z0-9-]{0,31}$`). Max 8 per user.
- `PUT /v1/browser/profiles/{id}` `{"name"}` → rename.
- `DELETE /v1/browser/profiles/{id}` → closes its session, deletes the
  directory; bots assigned to it fall back to `default`. `default` → 409.
- Bot assignment: `PUT /v1/bots/{id}` accepts `"browser_profile": "<id>" | null`
  (null = default). `GET /v1/bots/{id}` returns `browser_profile`.

Sessions: one Chrome per (user, profile). A bot's browser tools use the
bot's assigned profile. `max_sessions` counts per user.

## Sessions (server)

- `GET /v1/browser/sessions` → `{"sessions":[{"id"(= profile id),
  "profile_name","active","url"?,"title"?,"controller":"bot"|"user",
  "bot"?: bot id last driving it, "conversation_id"?: conversation of the
  last bot turn that used it, "updated_at"}]}`
- Per session: `GET /v1/browser/sessions/{id}/screenshot` (JPEG, same caps),
  `POST /v1/browser/sessions/{id}/takeover|handback|input|close`
  (same bodies and rules as the 0.14 single-session routes).
- The 0.14 routes `/v1/browser`, `/v1/browser/screenshot`, `/takeover`,
  `/handback`, `/input`, `/close` keep working and act on the `default`
  profile's session (back-compat for the shipped desktop).

## Bot settings (server)

`PUT /v1/bots/{id}` additionally accepts `display_name` (1-48 chars),
`description` (0-280), `model` (`{"provider","model"}` | null for default),
`browser_profile`. Avatar routes from 0.15 unchanged.

## Desktop

- Left rail: Chats, Browser, Approvals, Memory, Connectors, Vault, AI keys;
  Settings + account at the bottom. The separate "Bots" rail item is
  removed: bots are listed and created from the Chats sidebar.
- **Browser page**: grid of session cards (live thumbnail ~1 fps when
  visible, profile name, URL, which bot is driving, controller pill). Click a
  card → focus view: large live browser on the left, the conversation of the
  bot driving it on the right (read + reply), Take control / Give control
  back / Hide / End session. Manage profiles from this page (create, rename,
  delete, see which bots use each).
- **Bot settings** replaces the "Inspector": title is the bot's name; one
  editable form: picture, name, description, role/instructions, model,
  browser profile, capabilities (plain-language toggles). No raw ids or
  routing jargon outside "Advanced".
- **Vault page** (rail item "Vault"): the Passwords & keys list and form move
  here; the chat header button is removed.
