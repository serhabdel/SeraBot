# SeraBot 0.15.0: isolated Telegram, working connectors, simpler bots

## Telegram
- Telegram is its own conversation. Chats started in the desktop or mobile
  app are no longer mirrored to Telegram. The Telegram bot can still ask
  other bots and gets their answers back in Telegram. See `docs/telegram.md`.

## Connectors
- Outlook and Outlook Calendar use the same Microsoft app as sign-in and
  OneDrive (fixes `AADSTS900144 ... client_id`). Per-user tokens.
- Notion: paste your own integration token (checked before it is saved).
- Google Drive, Sheets, Calendar, Gmail: admin "Set up Google" card, then
  each user connects their own account. Steps in `docs/google-setup.md`.
- Anything not configured shows "Not set up yet" instead of a broken button.

## Approvals
- Only destructive tools ask first: `memory_delete`, `agentmail_send`,
  `telegram_send_document`, `onedrive_upload`. Everything else, including the
  browser, runs on the bot's judgement. See `docs/approvals.md`.

## Bots
- Custom picture per bot and per group.
- One-step "New bot" (name, picture, model, group) and "New group" (name,
  description, picture, members, rules, model). Ids are generated.
- A new bot opens a short interview, proposes its role and goal, and saves
  them when you confirm.
- Capabilities shown in plain words ("Use the web browser", ...).
- Bots get a described tool list every turn.

## Fixed
- "Rate limited (429)": signed-in users get a much larger budget, live
  polling has its own bucket, and the desktop backs off quietly.

## Operator steps
- Azure portal, existing app: add delegated Graph permissions
  `Mail.ReadWrite`, `Mail.Send`, `Calendars.ReadWrite`.
- Google: follow `docs/google-setup.md`.
