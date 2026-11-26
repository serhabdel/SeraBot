# SeraBot 0.74.25

## Highlights

- **Rich HTML Mail & Embedded-Logo Signatures (`mail.rs`)**:
  - Raised `MAX_SIGNATURE_HTML_CHARS` to 64 KiB so full Outlook/Gmail/Hostinger HTML signatures save cleanly.
  - Preserved safe `<style>` blocks in `sanitize_email_html` (`sanitize_style_sheet` strips `@import`, `expression()`, `javascript:`, `position: fixed`, and remote `url(...)` when remote images are blocked).
  - Automatically extracts `<img src="data:image/*;base64,...">` into `multipart/related` `cid:inline-N@serabot` MIME parts on send so pasted logos render in Gmail, Outlook, Apple Mail, and Hostinger.
  - Added `In-Reply-To` and `References` header support in `build_message_with_inline`.
- **AI Mail Management Tools (`mail.rs`, `gateway.rs`, `bots/hostinger.toml`, `bots/mailbox.toml`)**:
  - Upgraded `mail_send` with `body_html`, `cc`, `in_reply_to`, `references`, and automatic member HTML signature + inline logo attachment.
  - Added `mail_reply` (approval-gated in `DESTRUCTIVE_TOOLS`, threads via `In-Reply-To`/`References`), `mail_draft` (saves rich HTML drafts to IMAP `Drafts`), and `mail_triage` (read-only unread/folder/signature overview).
- **Desktop Rich HTML Editor, Reader CSP Fix & UI Upgrade (`ui_mail_reader.rs`, `ui_mail.rs`, `mail.css`)**:
  - Fixed `MailReader` to pass `*ctx.remote_loaded.read()` to `wrap_email_html_srcdoc_with_remote` so **Load remote images** actually displays remote images inside the sandboxed iframe.
  - Framed received HTML bodies inside a crisp `.mail-paper-card` surface and added **Draft HTML reply** to the Ask-bot strip.
  - Added **Visual / Paste** `contenteditable` editors in both `SignatureBuilderModal` (with live `cid:` → `data:` + remote-image preview) and `MailComposeModal` (with formatting toolbar, Visual / Source / Preview toggle, signature preview strip, and **AI Assist**).
