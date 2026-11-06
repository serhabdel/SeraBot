# SeraBot 0.14.0: new desktop design, server browser, passwords & keys

## Desktop
- New three-pane layout: labelled icon rail, chat list with search and
  last-message previews, chat column, and a browser panel that slides in
  while a bot browses.
- Every screen restyled with one design language: large titles, grouped
  cards, empty states with an action, loading skeletons, inline validation,
  press feedback, side-anchored panel motion, reduced-motion and
  reduced-transparency support.
- "New chat" lets you choose the bot. Account menu holds Settings and
  Sign out. Every icon button has a label or tooltip.
- Connectors: Microsoft first, OneDrive with Connect / Disconnect, one clear
  action per card, technical fields behind "Advanced".
- Bot actions in chat read as sentences ("Opened example.com", "Signed in
  with your saved login ...").

## Server browser
- Per-user headless Chrome on the server, driven by bots through gated
  tools: `browser_navigate`, `browser_snapshot`, `browser_click`,
  `browser_type`, `browser_press`, `browser_screenshot`, `browser_close`.
- Live view in the desktop app; **Take control** / **Give control back**.
  While you are in control the bot is paused.
- Public websites only: loopback, private, link-local, CGNAT and metadata
  addresses are refused, for the page and for every subresource and redirect.

## Passwords & keys (vault)
- One encrypted vault per user, shared by all of that user's bots.
- Bots see names only (`vault_list`). `browser_type` can type a saved
  username or password by name; the value never reaches the AI.
- Any vault value that shows up in tool output or an AI request is replaced
  with `[vault:<name>]`. Optional per-entry website allow-list.

## Fixed
- 0.13.1: Connectors showed "Unexpected API response".

## Operator steps
1. `google-chrome-stable` from Google's apt repo (Ubuntu's chromium is a snap
   stub that cannot run under the service sandbox).
2. Install `deploy/serabot-browser.conf` as
   `/etc/systemd/system/serabot.service.d/browser.conf`, then
   `systemctl daemon-reload`.
3. `[browser] enabled = true`, `provider = "spawn"`,
   `chromium_executable = "/usr/bin/google-chrome-stable"`, restart.
