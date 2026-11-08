# Server browser (per user)

SeraBot can run **one headless Chromium per active user profile** on the server. Bots drive it
through gateway tools; the owner watches and can take over from the desktop browser dock.
Page content is **untrusted data** and is scanned like any other tool output.

Disabled unless `[browser] enabled = true`.

## Configuration

```toml
[browser]
enabled = false
provider = "spawn"            # per-user Chromium started by SeraBot (spawn_cdp is an alias)
chromium_executable = ""      # empty: chromium, chromium-browser, /snap/bin/chromium, google-chrome
user_data_dir = ""            # base for profiles: <dir>/<user_id>; empty = <database dir>/browser
request_timeout_secs = 30     # per CDP command
max_sessions = 2              # concurrent browsers (1..16)
idle_timeout_secs = 600       # idle sessions are closed (30..86400)
no_sandbox = false            # only for a root service user / no user namespaces
```

`remote_cdp` and `browseros_neo` still parse but cannot back per-user sessions; startup
fails with a clear error if `enabled = true` with either. `remote_debugging_*` and
`browseros_neo_base` are ignored by `spawn` (the debugging port is chosen by the OS).

## VPS install (Ubuntu)

Ubuntu ships Chromium only as a snap: `sudo snap install chromium` (the apt packages
`chromium` / `chromium-browser` are transitional stubs that install the same snap).
Constraints of the snap: it needs `/snap/bin/chromium`, a service user with a real home
directory, and a profile directory it may write (set `user_data_dir` under that user's
home, for example `/home/serabot/browser-profiles`). If the snap is a poor fit, install
`google-chrome-stable` from Google's .deb and set `chromium_executable` to it. Run the
service as a non-root user so the Chromium sandbox stays on; only set `no_sandbox = true`
when you must run as root.

## Gateway tools

| Tool | Approval | Purpose |
| --- | --- | --- |
| `browser_navigate{url}` | yes | Open a public http(s) URL (starts the browser). |
| `browser_snapshot{}` | no | Text outline with element refs `e1..eN`. |
| `browser_click{ref}` | yes | Click a ref from the latest snapshot. |
| `browser_type{ref, text}` or `{ref, secret, field}` | yes | Type text, or a vault secret by name. A secret needs the owner's approval for the current page's host, and the entry must list that host. The value never reaches tool output; later page text is scrubbed to `[vault:<name>]`. See `docs/vault.md`. |
| `browser_press{key}` | yes | Enter, Tab, Escape, arrows, a single letter or digit, ... |
| `browser_screenshot{}` | no | JPEG stored as an artifact; not returned to the model. |
| `browser_close{}` | no | Close the session. |
| `browser_tabs{action?, tab?}` | no | List tabs (`t1`, `t2`, ...) or `switch`/`close` one by id. The active tab is the one the other tools act on; switching clears the refs. |
| `browser_new_tab{url}` | yes | Open a public http(s) URL in a new active tab (at most 8 tabs; same URL policy as `browser_navigate`). |

While the owner has taken over (`controller = user`) every bot tool, including
`browser_close`, fails with "the user is in control of the browser".

## Profiles and sessions (0.16)

A profile is a named, per-user Chrome user-data directory
`<browser base>/<user_id>/<profile_id>` with its own cookies and logins. Every user has a
`default` profile (created on first use, cannot be deleted) and at most 8 profiles in total.
Profile ids are generated from the name (`^[a-z0-9][a-z0-9-]{0,31}$`). One Chrome runs per
(user, profile); `max_sessions` is the server-wide cap on concurrent Chromes.

A bot's browser tools use the profile assigned with `PUT /v1/bots/{id}`
`{"browser_profile": "<id>"|null}` (null = `default`). Each session records the bot, and the
conversation, of the last bot tool call that used it.

Upgrading from 0.14/0.15: the previous single profile lived directly in `<base>/<user_id>`;
the new `default` profile starts empty in `<base>/<user_id>/default` (logins must be redone).

## Owner API

Profiles: `GET /v1/browser/profiles` → `{"profiles":[{"id","name","is_default","bots":[bot
ids],"created_at","last_used_at"?}]}`; `POST /v1/browser/profiles` `{"name"}` → one profile
(`409` past 8); `PUT /v1/browser/profiles/{id}` `{"name"}` → rename; `DELETE
/v1/browser/profiles/{id}` → `{"deleted":true,"id","reassigned_bots":[...]}` after closing its
session, deleting its directory and reassigning its bots to `default` (`default` → `409`).

Sessions: `GET /v1/browser/sessions` → `{"sessions":[{"id"(= profile id),"profile_name",
"active","url"?,"title"?,"controller":"bot"|"user","bot"?,"conversation_id"?,"updated_at"}]}`,
one entry per profile (`active:false` when no Chrome is running). Per session:
`GET /v1/browser/sessions/{id}/screenshot` (JPEG, 1280 px wide, at most 512 KiB, 404 without a
session) and `POST /v1/browser/sessions/{id}/takeover`, `/handback`, `/input`
(`click{x,y}`, `text{text}`, `key{key}`, `scroll{dy}`, `navigate{url}`; 409 unless the user
holds control), `/close`. An unknown profile id is `404`.

The 0.14 routes `GET /v1/browser`, `GET /v1/browser/screenshot`, `POST /v1/browser/takeover`,
`/handback`, `/input`, `/close` are aliases for the `default` profile's session. All routes act
on the authenticated user's profiles and sessions only.

## Navigation guard and its limits

Only `http` and `https`. Refused: embedded credentials, internal names (`localhost`,
`*.local`, `*.internal`, single-label hosts) and any non-public address: loopback,
private, link-local (including 169.254.169.254 metadata), CGNAT 100.64/10, unspecified,
multicast, reserved and documentation ranges, and the IPv6 equivalents including
IPv4-mapped, NAT64 and 6to4 forms. Names are resolved and **every** address must be public.
The same check runs on every subresource and redirect hop via CDP `Fetch.requestPaused`;
blocked requests fail with `AccessDenied`. Popups are closed, downloads denied, JavaScript
dialogs dismissed.

Limits: Chromium resolves names itself, so a DNS-rebinding answer that changes between
our check and the browser's lookup is not caught (verdicts are cached 30 s per origin);
WebSocket, WebRTC and service-worker traffic bypass the Fetch domain. Block egress from
the service user to private ranges at the host firewall as defence in depth.

## Other options

Firecrawl for plain extraction (see `docs/firecrawl.md`); BrowserOS neo on the owner PC
for logged-in flows is out of scope for this server browser.

## Tabs and pointer (0.18)

`GET /v1/browser/sessions` entries (and the takeover, handback, input and tab replies, which also carry `id` and `profile_name`) include `tabs` (`[{id: "t1", url, title, active}]`, at most 8) and `pointer` (`{x, y, action: "click"|"scroll", seq, vw, vh}`, viewport pixels, `seq` grows per action). While the user holds control: `POST /v1/browser/sessions/{id}/tabs {"url"}` opens a tab (public http(s) only), `POST .../tabs/{tab}/switch` and `.../tabs/{tab}/close` switch or close one; the last tab cannot be closed. The desktop Browser page draws a tab strip above the live view and a cursor with a click ripple (static under reduced motion) while the bot drives.
