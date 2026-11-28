# Release 0.74.33 — Claude Agent SDK gateway sign-in that actually finishes

A fix batch for **Sign in with browser** on the Claude Agent SDK (Gateway) card.

## What was wrong

The gateway started `claude auth login --claudeai` with `BROWSER=true`. The CLI
hands its *automatic* sign-in URL (which redirects to the CLI's own
`http://localhost:<port>/callback`) to `$BROWSER` and only *prints* a manual URL,
whose last page shows a `code#state` to paste. With `BROWSER=true` the automatic
URL was thrown away, the desktop opened the manual one, and every sign-in ended
on a page with a code while the sheet said "local gateways finish
automatically" and waited until it timed out. A pasted code without the `#`
also made the CLI re-prompt, so the request hung for 20 s and then killed the
flow; polls racing a paste hit a 404; and refusals came back as a generic
"claude auth login failed".

## Gateway (`services/claude_gateway`)

- `$BROWSER` is a one-shot capture script (POSIX): `POST /v1/auth/start`
  returns the automatic `url`, the `manual_url`, the CLI's `callback`
  `{port, path}` and its `state` (`mode: "callback"`; `"paste"` on Windows).
- `POST /v1/auth/complete` with `state` hands a caught redirect to the CLI's own
  loopback listener (state checked in constant time; only the port and path the
  CLI announced; no proxy). A pasted `http://localhost…/callback?…` address is
  relayed the same way.
- A paste the CLI asks for again returns `code_incomplete` and the flow stays
  open; a refused code returns the CLI's own reason with the code masked.
- Finished flows keep answering `GET /v1/auth/flow/{id}` until they expire.
- The printed URL is only taken once complete; codes over 4096 characters or
  with control characters are refused before reaching the CLI's terminal.

## Server (`crates/serabot`)

- `signin/start` for `claude-agent-sdk` returns `manual_url`, `callback_port`,
  `callback_path` and `state` (privileged ports or odd paths are dropped).
- `signin/complete` accepts `state` for a caught redirect.
- Status polls no longer reach the gateway while a complete owns the flow; a
  flow the gateway forgot reads as `expired`; `code_incomplete` re-opens the
  flow; Claude's refusal text becomes the denial reason.

## Desktop (`crates/serabot-desktop`)

- Opens the automatic URL. If the CLI's callback port is free on this computer
  (gateway on the server), the desktop catches the redirect and relays it; if
  the gateway runs here, the CLI receives it directly and the poll finishes.
- Folded fallback: **Browser shows a code, or cannot reach localhost?** with
  **Open code page** and a paste box (`code#state` or the localhost address).
- The **Sign in with browser** tile for an already-saved gateway provider now
  starts the sign-in instead of opening an idle sheet.

## Verification

Checked against the real `claude` CLI 2.1.x with synthetic codes only (relay,
re-prompt, pasted address, refusal text). A full sign-in with a real Claude
account has **not** been run here; the owner should run one before relying on
it.
