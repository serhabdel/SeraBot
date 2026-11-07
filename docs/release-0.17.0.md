# SeraBot 0.17.0

## Chat
- Live activity in chat: "Thinking…", then one row per step ("Opening example.com", "Asking Analyst"), folding into an "N steps" summary when the reply lands. The "Queued" row is gone.
- Inline Approve / Deny for steps waiting on approval (goes through the existing gateway approval API).
- Server records UID-scoped `turn.step` events; labels are built from the tool name and URL host only and pass the blocklist.
- Ordered lists keep their numbering (previously every item showed "1.").
- Bot avatar upload fixed: the file picker filter now lists `.png,.jpg,.jpeg,.webp` (MIME tokens produced an empty dialog filter in dioxus-desktop).

## Design
- Light theme with a violet accent is the default; dark theme retuned to match; "Same as my computer" option.
- Model picker is a compact chip in the composer.

## Settings and Usage
- Settings has a section list: Account, Appearance, Notifications, Chat, Telegram, Voice, Admin (admins only), About.
- New Usage page: AI replies, tokens, tool runs, per-day chart, tables by model and bot; 7/30/90 days.
- `GET /v1/usage` now returns `per_model`, `tools` and `scope`. `scope=all` is admin-only totals with no content.
- Estimated cost shows "Not available" (the server stores no prices).

## Composio
- Per-user Composio API key (validated, stored in the per-user secret store, never returned).
- Connectors card: search apps, Connect / Disconnect.
- Bot tools `composio_search_tools`, `composio_connect`, `composio_execute`; approval only for destructive or sending action slugs.

## Not verified
- New screens have not been viewed in the running app.
- Composio has not been run against the real service; tests use a mock server.
