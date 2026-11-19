# Release 0.63.0

WhatsApp done the way the owner described it, plus desktop cleanup: the model
picker leaves the composer, the bot settings panel is fixed, the bots sidebar
is grouped, and each bot gets a Library. Tests use synthetic fixtures only.
Nothing here ran against a live WhatsApp or Telegram account, and the desktop
changes were not rendered on Windows in this session.

## WhatsApp

- **One bot speaks on WhatsApp.** Only the WhatsApp bot (`[whatsapp_link]
  bot`) gets WhatsApp tools. Every other bot, including main with
  `tools = ["all"]`, loses them on every turn. This fixes bots trying the
  Business API ("Checking WhatsApp templates… not connected") for a linked
  personal number.
- **Business is separate.** The Business (Cloud API) tools only appear for the
  WhatsApp bot once WhatsApp Business is connected. In Integrations, there are
  now two cards: **WhatsApp** (personal number, linked device) and
  **WhatsApp Business**.
- **Talk to every bot through WhatsApp.** The WhatsApp bot asks any other bot
  with `ask_teammate` and passes the answer back in the same chat.
- **Files you ask for come back.** New `whatsapp_link_reply_file` sends one of
  your files into the WhatsApp chat the conversation came from. It takes no
  recipient, checks that you own the file, passes the blocklist, and is not
  approval-gated, like the automatic text reply. Messages the bot starts to
  other allowed chats still need approval.
- **Limits.** Allowlist only, no groups, 6 messages per chat per minute, and a
  new `sends_per_day` cap (default 100) across all chats.
- **Device name.** The linked device registers as Chrome on `device_os`
  (default Windows) instead of the library default. This applies at pairing,
  so **sign out and link again** to see it. It does not reduce the ban risk.
- Design: `docs/specs/whatsapp-linked-device.md` § 0.63. AGENTS.md updated.

## Telegram

- New `telegram_reply_file`: in a conversation that came from Telegram, a file
  you asked for goes back to your Telegram chat without an approval prompt.
  `telegram_send_document` from other conversations still asks first.

## Desktop

- **Model picker in the header.** A model chip in a one-to-one chat's header
  opens the full picker (model, account, thinking) as a popover. The composer
  keeps only writing, files, voice and send.
- **Bot settings.** "Memory refresh" moved into the scrolling form as a
  collapsible **Memory** section, so it no longer sits under the Save bar.
- **Bots sidebar.** Bots are ordered Pinned, then Recent (newest message in the
  last 7 days, or unread), then All bots A–Z, with quiet sub-headings.
- **Bot Library.** The bot panel's Files tab is now **Library**. It covers
  every file in the bot's chats, both made by bots and uploaded by you
  (`GET /v1/bots/{bot}/library`, per user), with:
  - search, kind filters (Made by bots, Uploaded, Media, Documents), date
    filters (Today, 7 days, 30 days), and sort;
  - date group headings;
  - an item dialog with its date, source, chat and size, and actions: Open,
    Show in Explorer, Save to Downloads, Go to chat.
