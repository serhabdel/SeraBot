# SeraBot 0.23.0

## Built-in browser
- Popups and links that open a new window become tabs instead of being closed silently. Each new tab gets the same network guard before it runs. Tabs past the limit (8), or pages not opened by one of your tabs, are closed without ever being resumed.
- The tab strip always shows every tab. While you drive you can switch, close and open tabs. Grid cards show the tab count and how long the session has been idle.
- Driving toolbar: back, forward, reload and an editable address bar. Every navigation still goes through the URL guard.
- When Google refuses a sign-in ("This browser or app may not be secure"), the session shows a banner. It points you to the Google connector, or to signing in from a normal browser on your PC. The bot is told not to retry the login. SeraBot does not try to evade the provider's check.
- Defence in depth against link prefetch: DNS prefetch and network prediction are off in every browser profile. OS-level egress rules for the service user are still recommended (see `docs/browser.md`).

## Vault and password managers
- Bots can ask for a credential with `vault_request_secret`. You type the value into a secure card. It goes straight into your encrypted vault, and the bot only learns that it was saved.
- Typing a saved secret into a page always needs your approval for that exact site. "Always" approvals are downgraded to "Once" for secret fills. Auto-approve applies only when the page host matches the entry's sites. Fills happen only on HTTPS (or localhost). They stop if the page moved after approval.
- Entries with no sites can no longer be typed anywhere; add the sites they belong to.
- 1Password and Proton Pass references: an entry can point at `op://vault/item/field` or a Proton Pass item instead of storing a value. The value is read at fill time by the provider's CLI on the server, using your own token, and is never stored. Proton Pass is off by default (`[vault] protonpass_enabled`) until your `pass-cli` version is verified.
- "Import from this PC" lists your 1Password or Proton Pass logins with the CLI on your computer and creates references, never copies of the values.
- See `docs/vault.md`.

## Chat
- Question cards: when a request is ambiguous, a bot asks with a card that has 2-4 options and a free-text answer, instead of guessing. With auto-approve on, or a goal set for the chat, the bot decides and states its assumption instead. Bots never ask for passwords or keys in a question.
- Secret request cards for credentials (see Vault).
- Answers and dismissals post as your message, so the bot carries on in the same chat.

## Bots
- Every shipped bot has a human title and a one-line description: Main, Analyst, Auditor, Accountant (`comptable`), Courier, Inbox, Legal, Maker, Marketer, Markets, Watcher and WhatsApp.
- Starter prompts for every bot, including Auditor, Accountant, Marketer, Markets, Courier, Inbox and WhatsApp.
- "Meet your team" lists every shipped bot with its picture and description, and adds back any you removed.

## Not verified
- No live run against Google, 1Password or Proton Pass. Tests use synthetic fixtures, a fake CLI and local `data:` pages (the live browser tests ran against local Edge).
- The Proton Pass CLI flags and listing JSON shape are unverified for your installed version.
- The new desktop screens were compiled and unit-tested but not looked at on screen.
