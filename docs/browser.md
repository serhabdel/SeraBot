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

## Smarter actions (0.70)

Ideas borrowed from agent-browser, built into the Rust browser (no Node, no extra Chrome).

| Tool | Approval | Purpose |
| --- | --- | --- |
| `browser_snapshot{mode}` | no | `interactive` (actionable elements only), `full` (default) or `text` (readable main text, bounded, no refs). |
| `browser_find{role?, name?, text?, limit?}` | no | Fresh snapshot, then refs by ARIA role and accessible name, or lines by visible text. |
| `browser_wait{text \| role/name \| state, timeout_secs?}` | no | Wait up to 30 s for text, an element, `load` or `networkidle`. A timeout is a normal answer, not an error. |
| `browser_select{ref, option}` | yes | Choose a `<select>` option by label or value. |
| `browser_check{ref, checked?}` | yes | Tick or untick a checkbox, radio or switch; verified after the click. |
| `browser_scroll{direction?, amount?, ref? \| to_ref}` | no | Scroll the page or an element, or bring a ref into view. |
| `browser_hover{ref}` | no | Pointer move only. |
| `browser_upload{ref, artifact}` | **owner approval** | Sets a file input to a member-owned artifact through a temp file under `<browser dir>/.tmp/<user>`, deleted right after. The approval names the file and the site and binds the artifact hash and the page host. |
| `browser_download{ref, timeout_secs?}` | yes | Click, wait for the download (25 MB cap), save it as an artifact. Downloads are refused again afterwards and the temp file is deleted. |
| `browser_pdf{}` | no | `Page.printToPDF` saved as an artifact (25 MB cap). |
| `browser_dismiss_banners{}` | yes | Cookie and consent banners. |
| `browser_forget_logins{}` | **owner approval** | Closes the browser and deletes the member's profile directory (cookies, local storage). |

**Actionability.** A click, hover, type, select, check or upload first resolves the node, then checks it is
attached, visible (non-zero box, not `visibility:hidden` or `display:none`), enabled (`:disabled`,
`aria-disabled`), editable (typing), scrolled into view and, for a click or hover, not covered:
`elementFromPoint` at the box centre must land on the node, a descendant or its own label. A failure is
an error such as "the button is covered by a cookie banner" or "the field is disabled", never a
silent success. The point test is skipped inside iframes.

**Banners.** After `browser_navigate` and `browser_new_tab` (when `[browser] auto_dismiss_banners = true`, the
default) and on `browser_dismiss_banners`, buttons inside known consent containers (OneTrust, Cookiebot,
Didomi, TrustArc, Usercentrics, Quantcast, and any dialog or fixed box that talks about cookies) are
ranked in Rust: reject non-essential, then accept only necessary, then a plain accept (last resort).
English, French, Arabic and Spanish texts are covered. Settings buttons, submit buttons in other forms
and anything that looks like a payment, sign-up or login are never clicked.

**Light pages.** `[browser] block_heavy_resources = false` (default) or `browser_navigate{light: true}` per
session skips images, media and fonts through the same `Fetch.requestPaused` hook that runs the guard.

**Logins stay.** Each member's profile directory (`<browser dir>/<user>/<profile>`) is kept between sessions,
so cookies and local storage survive restarts and idle reaping. It is never shared between members;
`browser_forget_logins` removes only the acting member's profile for the bot's assigned profile.

CDP used: `DOM.resolveNode`, `DOM.getBoxModel`, `DOM.scrollIntoViewIfNeeded`, `DOM.setFileInputFiles`,
`Runtime.callFunctionOn`, `Runtime.evaluate`, `Input.dispatchMouseEvent`, `Page.printToPDF`,
`Browser.setDownloadBehavior`, `Browser.cancelDownload`, `Browser.downloadWillBegin`,
`Browser.downloadProgress`, `Fetch.failRequest` (heavy resources).

While the owner has taken over (`controller = user`) every bot tool, including
`browser_close`, fails with "the user is in control of the browser".

Page tool replies carry the URL and title. When the page is a provider's sign-in refusal (see
below), the reply also carries a plain `note:` telling the model not to retry the login and to
suggest the connector.

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
blocked requests fail with `AccessDenied`. Downloads are denied and JavaScript dialogs dismissed.

Popups: every page the browser creates attaches to us paused (browser-level `Target.setAutoAttach`
with `waitForDebuggerOnStart`). A page one of our tabs opened (`window.open`, `target=_blank`) is
configured with the same guard, then runs and is adopted as a tab that becomes the active one
(`browser/chromium.rs`, `should_adopt`). A popup from any other page, or any popup while the
browser already has 8 tabs, is closed and logged. A page that is not adopted is closed while it is
still paused, so its code never runs. The owner's `+` and the bot's `browser_new_tab` create a blank
page with `Target.createTarget`; no script runs in the active tab, so a site cannot change how the
new tab opens. The new page is reserved (it counts toward the limit but no bot call reaches it),
configured with the guard, then made the active tab before the guarded navigation.

Preconnect and DNS prefetch are switched off as defence in depth, not as the control: the
`--dns-prefetch-disable` switch, `NetworkPrediction` in the disabled features, and
`net.network_prediction_options = 2` written into the profile's `Default/Preferences` before each
launch (other keys in that file are kept). Limit, measured against a real Chromium
(`live_popup_first_request_is_guarded`): when a popup is opened with a URL, Chromium can still open a
bare TCP connection to that address before the guard sees the request. The test found no HTTP
request reaching a refused loopback address. The bare connection can still reveal that a port is
open, so the host firewall must block the service user's egress to private ranges (see above), as
already advised; OS egress rules remain recommended.

Limits: Chromium resolves names itself, so a DNS-rebinding answer that changes between
our check and the browser's lookup is not caught (verdicts are cached 30 s per origin);
WebSocket, WebRTC and service-worker traffic bypass the Fetch domain. Block egress from
the service user to private ranges at the host firewall as defence in depth.

## Other options

Firecrawl for plain extraction (see `docs/firecrawl.md`); BrowserOS neo on the owner PC
for logged-in flows is out of scope for this server browser.

## Tabs and pointer (0.18)

`GET /v1/browser/sessions` entries (and the takeover, handback, input and tab replies, which also carry `id` and `profile_name`) include `tabs` (`[{id: "t1", url, title, active}]`, at most 8) and `pointer` (`{x, y, action: "click"|"scroll", seq, vw, vh}`, viewport pixels, `seq` grows per action). While the user holds control: `POST /v1/browser/sessions/{id}/tabs {"url"}` opens a tab (public http(s) only), `POST .../tabs/{tab}/switch` and `.../tabs/{tab}/close` switch or close one; the last tab cannot be closed. The tab list is part of every session entry (`GET /v1/browser/sessions`), so there is no separate list route. The desktop Browser page draws a tab strip above the live view and a cursor with a click ripple (static under reduced motion) while the bot drives. Without control the tabs are shown read-only. The grid card shows the tab count.

### Driving controls (0.23)

While the user holds control, `POST /v1/browser/sessions/{id}/input` also takes `{"type":"back"}`,
`{"type":"forward"}` and `{"type":"reload"}` for the active tab. The landing page gets the same
public-address check as bot navigation (a private landing is reset to `about:blank`). A
`{"type":"navigate","url"}` input goes through the same URL guard as `browser_navigate`.

Each session entry also has `idle_secs` (seconds since the last bot or owner action; polling does
not count) and, when the active tab shows a provider's sign-in refusal, `signin_blocked:
{"provider","message"}`.

## Sign-in refusals (0.23)

Some providers refuse sign-in in a browser they consider automated. Google answers a sign-in from
such a browser with `accounts.google.com/.../signin/rejected` ("This browser or app may not be
secure"). SeraBot does not try to get past that: no user-agent spoofing and no webdriver patching.
`browser/signin.rs` recognises the refusal pages in a small table (one row per provider, https
only). When the active tab lands on one, the session reports `signin_blocked`, the desktop shows a
banner with an "Open Connectors" button, and bot tool replies carry the refusal note. Use the
Google connector (Connectors → Google) for Gmail, Drive and Calendar, or sign in from a regular
browser on the owner's PC.

## Accounts and anti-captcha (0.74.36)

- `GET /v1/browser/sessions/{id}/accounts` and the bot tool `browser_account_status` report which
  external accounts a profile is signed into (Google, YouTube, LinkedIn, Instagram, X, Facebook),
  read from cookie **names and domains only — never values**. Audited as `browser.accounts`.
- Google is now in the Social connect-in-browser flow (`POST /v1/browser/social/open`), so the
  owner can complete one Google sign-in in the Browser tab. The refusal-page rule above is
  unchanged: a shown captcha is never retried or bypassed.
- `[browser] hide_automation = true` (default) adds `--disable-blink-features=AutomationControlled`
  so `navigator.webdriver` reads undefined; this lowers captcha frequency without bypassing any.

## Live view performance (0.74.36)

- `GET /v1/browser/sessions/{id}/frames` streams live JPEG frames over SSE from a CDP
  `Page.startScreencast` (≤ 512 KiB per frame). Polling clients keep using
  `GET .../screenshot`, which now accepts `?q=` (20..=90) to cap the starting JPEG quality.
- The desktop live view is adaptive: while the bot drives it polls every 1200 ms at quality 50;
  while the owner holds control, every 220 ms at quality 70. Over a Cloudflare tunnel this cuts
  live-view traffic several-fold during bot-driven browsing.

## Hide and idle sessions

Hiding the Browser page or the dock never closes a browser: `Hide` only clears the focus view and
stops the pictures. A session ends when the owner presses End session, or when it has been idle
for `idle_timeout_secs` (no bot tool call and no owner input). The grid shows how long it has been
idle.
