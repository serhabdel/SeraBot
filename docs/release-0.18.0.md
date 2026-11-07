# SeraBot 0.18.0

## Chat
- Goal and to-do panel at the top of a chat, collapsed by default ("N of M done"). Bots record it with the new `plan_update` tool (no approval) and work the list to the end without extra questions.
- Per-chat Auto-approve (YOLO) switch. Calls that would ask are approved automatically in that chat only, still through the gateway, policy and blocklist, and audited as `approval.auto_approved`. Blocklisted arguments still wait for the owner.
- File attachments in the composer (25 MB, images, pdf, office, txt/md/csv). Files land in the user's inbox; text files and text PDFs are excerpted for the bot.
- Media cards with Open and Save as for files the bot produced.

## Browser
- Take control fixed: takeover, input and handback replies now carry the session id and profile name the desktop expects.
- Tabs: up to 8 per session, tab strip in the Browser page, bot tools `browser_tabs` and `browser_new_tab`.
- Visible bot pointer with a click ripple while the bot drives (static under reduced motion).
- Bots are told never to show element refs such as `e12` to the user.

## Models
- The model picker lists only models the save check accepts (allowlist intersected with a fresh verified catalog). Fixes "The server rejected that model selection" (422).
- Empty list message: "Ask the admin to allow more models."

## Local folders
- Desktop Settings > Local folders: allowlist of folders, read-only or read-write, stored only on the PC.
- Bot tools `local_list`, `local_read`, `local_write` relay through the gateway to the connected desktop, which re-checks the allowlist (no `..`, symlinks, junctions, secrets files). `local_write` needs approval.

## Security
- `GET /v1/artifacts/{hash}` is now owner-scoped.

## Not verified
- New screens have not been viewed in the running app.
- Tabs and pointer were tested against the fake browser backend only.
- Images are stored but not yet sent to vision models.
- Existing users' bots need the new tools added (`browser_tabs`, `browser_new_tab`, `local_*`).
