# SeraBot 0.16.0: Browser page, profiles, bot settings, Vault page

## Desktop
- **Bot settings** replaces the Inspector: picture, name, description,
  instructions, model, browser profile and abilities in one form. Ids and
  routing live under "Advanced". Switch off and delete (with confirm) from
  the chat list or the settings panel; the main bot cannot be switched off.
- **Browser** page in the left bar: one card per open browser with a live
  thumbnail, the bot driving it and who is in control. Click a card for a
  big view with that bot's chat beside it; take control, give it back, hide,
  or end the session. Manage browser profiles on the same page.
- **Vault** page in the left bar; the "Passwords & keys" button left the
  chat header.
- The "Bots" item left the left bar; bots live in the chat list.

## Server
- Named browser profiles per user (up to 8), each with its own logins and
  cookies. Assign one per bot, or use Default.
- `/v1/browser/sessions` and per-session routes; the 0.14 routes act on the
  Default profile.
- `PUT /v1/bots/{id}` takes `display_name`, `description`, `model`,
  `browser_profile`.

## Note
- The new Default browser profile starts empty: sign in again inside the
  bot browser once.

## Branding
- New mascot logo, app icons, landing, privacy and terms pages; README.
