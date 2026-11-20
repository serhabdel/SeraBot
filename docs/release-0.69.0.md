# Release 0.69.0

Your bots can now edit images and turn photos into PDFs. On WhatsApp the
WhatsApp bot does this itself, and files can be sent to your own chats
without an approval card. There is also cost tracking with a monthly budget,
and a weekly digest. Tests use synthetic fixtures. Nothing here ran against a
live WhatsApp, Telegram or provider.

## Image tools

These tools run on the server. Each one makes a new file; the original is
never changed.

| Tool | What it does |
| --- | --- |
| `image_crop` | Crop by pixels or percent, to an aspect (`1:1`, `4:3`, `16:9`, `a4`), or `trim=true` to cut flat borders |
| `image_rotate` | Rotate by 90, 180 or 270 degrees, and/or flip horizontally or vertically |
| `image_adjust` | Change brightness and contrast, make it grayscale, or give it a clean black-and-white "scanned page" look |
| `image_compress` | Shrink the file to a target KB, optionally with a longest side in pixels |
| `images_to_pdf` | Make one PDF from 1–30 images, one per page; A4, Letter or fit-to-image; auto, portrait or landscape; set margins |
| `pdf_to_images` | Take the page pictures out of a scanned or photographed PDF |

The tools work on files your bots made, and on the photos you send in a chat.
On WhatsApp, Telegram or the console you no longer need to upload a photo
again.

Re-encoding removes metadata such as GPS. The photo is turned upright from its
EXIF orientation first.

## WhatsApp: photos and PDFs

- **The WhatsApp bot does the work itself.** It now has the image, PDF and
  OCR tools, so "crop this and make it a PDF" happens in the chat instead of
  being passed to another bot.
- **Several photos become one message.** Photos sent within 8 seconds of each
  other reach the bot together (up to 10), so "one PDF with all of them"
  works.
- **It looks at each photo.** It says what it is (a receipt, a document page,
  a screenshot) and offers what fits: crop it to the page, make a PDF, or read
  the text.
- **Results come back in the chat.** It can also send them to another of your
  allowed chats when you ask.

## Send files to yourself without approval

- **Telegram:** when you ask for a file, `telegram_send_document` sends it to
  your own Telegram chat without an approval card. It only ever goes to your
  own linked chat.
- **Switch:** "Send files to my own chats without asking", under Usage →
  Alerts. It is on by default. Turn it off to get the approval card back.
- **Limits:** 50 a day, after which approval is asked again. Each send is
  audited as `delivery.own_chat`.
- **WhatsApp:** `whatsapp_link_reply_file` now takes an optional `to`, which
  must be one of your allowed chats. Sending anywhere else still needs
  approval.

## Costs and budget

- **Prices:** the owner keeps a price table in USD per million input and
  output tokens. One row per model, or `*` for a provider-wide price. Edit it
  in Usage → Costs this month → **Edit prices**.
- **Your estimate:** each member sees an estimated cost for this month, per
  model. Models without a price are counted as "tokens without a price"
  instead of being guessed.
- **Budget:** you can set a monthly budget. You get an alert at 80% and again
  at 100%, once each per month. Bots keep working either way.
- **Estimates only:** your provider's own bill is the real figure.

## Weekly digest

- Every Monday at 08:00 UTC you get a short message on your alert channels:
  - replies and tokens;
  - estimated cost against your budget;
  - your most used model;
  - failed tasks, failovers and expired sign-ins;
  - for the owner, the last backup and its test restore.
- Turn it off under Alerts, or press **Send me the digest now** to preview it.
- Set the day and time with `[ops] digest_weekday`, `digest_hour_utc` and
  `digest_enabled`.

## Desktop

- **Try another way:** failed steps have a new "Try another way" button. It
  asks the bot to change its approach once, following the error's hint,
  instead of repeating the same call.
- **PC images:** the maker bot can edit an image on your PC. It imports it,
  edits it, and gives the new file back in the chat or on OneDrive. Bots
  never write to your PC, so the original is never touched.

## API

| Endpoint | Access | What it does |
| --- | --- | --- |
| `GET /v1/usage/costs` | the caller | this month's estimate, your budget, the price table |
| `PUT /v1/usage/budget` | the caller | set your budget; `{"monthly_usd": null}` clears it |
| `PUT /v1/admin/prices` | owner only | replace the price table |
| `POST /v1/alerts/digest` | the caller | send me my digest now |
| `PUT /v1/alerts` | the caller | now also accepts `auto_send_files` and `weekly_digest` |
