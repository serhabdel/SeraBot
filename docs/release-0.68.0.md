# Release 0.68.0

Alerts that reach you, nightly backups you can check, voice replies on
WhatsApp, clearer office-file steps, and a phone app that catches up. Tests
use synthetic fixtures. Nothing here ran against a live Telegram, WhatsApp,
OneDrive, speech provider or phone.

## Alerts

SeraBot now tells you about its own problems. It sends to your linked Telegram
chat, and also to one WhatsApp chat if you pick one (only your own allowed
chats can be picked).

| Alert | Who gets it | When |
| --- | --- | --- |
| Model failing over | the member | one model failed over 3 times in 15 minutes |
| Sign-in expired | the member | a provider account needs a new sign-in |
| PC tunnel dropped | the member | the desktop relay went offline while a turn was running |
| Disk nearly full | owner | the data disk passes 90% |
| Memory high | owner | server memory passes 92% |
| Backup failed | owner | the nightly backup or its test restore failed |

Alert rules:
- The same alert is sent at most once every 6 hours.
- Each alert is audited as `alert.sent`. Alerts never include message text.

Where to set it up: **Usage → Alerts and backups**. The card has the Telegram
switch, the WhatsApp chat picker, and your last alerts.

Configure the thresholds in `[ops]` (`disk_alert_percent`,
`memory_alert_percent`, `failover_alert_count`, `alert_cooldown_mins`).
`alerts_enabled = false` turns alerts off.

## Nightly backup

Every day at `backup_hour_utc` (default 03:00), the server:

1. Takes a consistent copy of the database (`VACUUM INTO`).
2. Encrypts it with the server's age key into `data/backups/serabot-<time>.db.age`.
3. Test restores it: decrypts the copy to a temporary file, then runs `PRAGMA integrity_check` and counts the members.
4. Copies it to your OneDrive `Backups` folder, if OneDrive is connected.
5. Keeps the last 7 backups (`backup_keep`).

The unencrypted copy never stays on disk. To restore a backup you need the
same age identity (`private/age-identity.txt`), so keep that file safe.

The owner's Backups card shows:
- the last backup, its size and whether it reached OneDrive;
- the test-restore result;
- the files kept;
- a **Back up and test now** button.

A failed backup also sends an alert.

## Voice replies

- **New `/auto` mode** (Telegram and WhatsApp): the bot answers a voice note
  with voice and a typed message with text. `/voice` always answers with
  voice, and `/text` always answers with text.
- **WhatsApp voice replies:** WhatsApp now gets real audio replies, followed by
  the same text so you can read and search it.
- **No server speech provider?** Speech then uses your own connected Mistral
  account, the same rule as voice-note transcription. A voice id
  (`audio.voice_id`) is still needed.

## Office files: clearer steps and errors

- **Clear names:** each Word, Excel, PowerPoint and PDF step now says what it
  does, for example "Editing a spreadsheet", "Reading a Word document" or
  "Merging PDFs".
- **What is changing:** the step card shows what the step changes, for example
  "12 changes: 8 values, 3 formulas, 1 charts", "Cells A1:D20" or
  "Blocks 1–40". It uses fixed words, counts and checked ranges only, so cell
  values and text never appear.
- **Errors with a next step:** a failed file step now says what to do next.
  The hint is shown on the card and also given to the bot, so it does not
  retry the same thing unchanged. It covers:
  - a file open in Excel, Word or OneDrive;
  - a file that was moved or renamed;
  - a location SeraBot is not allowed to use;
  - a name that is already taken;
  - a file that is too big;
  - a password-protected file;
  - macros;
  - a damaged file;
  - your PC being offline;
  - a timeout;
  - an unsupported file type.
- **Desktop local files:** errors now say why the file couldn't be used (moved
  or deleted, access refused, open in another program, disk full, slow drive)
  instead of only "could not be opened".
- **Bot instructions:** the maker, analyst and comptable bots now look at a
  file before changing it, edit in one batch, say exactly what changed, and act
  on a file error's hint once. The maker prompt also no longer says workbooks
  can't get charts.

## Phone app 0.68

- **Usage insights:** channels, PC tunnel, model speed, and (owner) server
  load.
- **Bot Library:** per bot, with filters, search and date groups.
- **Memories in reply traces:** each reply's trace shows the memories it used
  and saved.
- **Approval alerts:** a banner with a Review button, haptic feedback, and a
  badge. These are in-app only; real push notifications need setup that isn't
  done yet.

## API

| Endpoint | Access | What it does |
| --- | --- | --- |
| `GET /v1/alerts` | the caller | your alert settings, WhatsApp choices, recent alerts and thresholds |
| `PUT /v1/alerts` | the caller | save your alert settings |
| `GET /v1/admin/backups` | owner only | backup status and files |
| `POST /v1/admin/backups/run` | owner only | start a backup now; it runs in the background |
