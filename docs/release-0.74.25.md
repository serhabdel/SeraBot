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
  - Framed received HTML emails inside a crisp `.mail-paper-card` surface and added **Draft HTML reply** to the Ask-bot strip.
  - Added **Visual / Paste** `contenteditable` editors in both `SignatureBuilderModal` (with live `cid:` → `data:` + remote-image preview) and `MailComposeModal` (with formatting toolbar, Visual / Source / Preview toggle, signature preview strip, and **AI Assist**).
- **Compliant Claude Setup (October 2026 Policy)**:
  - Removed the shut-down third-party `claude.ai` OAuth PKCE sign-in from the `anthropic` preset and `/v1/providers/anthropic/auth/start|finish`, leaving **Anthropic (API Key)**, **Claude Agent SDK (Gateway)** (`services/claude_gateway`), and **Claude Code (Desktop ACP)**.
- **Antigravity (`pi-antigravity` v0.15.0) Protocol Overhaul (`cloud_code.rs`, `thinking.rs`, `relay_net.rs`, `provider_presets.rs`)**:
  - Added `https://www.googleapis.com/auth/aicode` to `ANTIGRAVITY_SCOPES` and updated the default User-Agent to `antigravity/cli/1.5.5` (with `ANTIGRAVITY_USER_AGENT` override).
  - Implemented `pi-antigravity`'s 126-character `systemInstruction` auto-folding (`ANTIGRAVITY_INLINE_SYSTEM` override) into the first user turn in `contents` to eliminate false `429 RESOURCE_EXHAUSTED` errors on consumer endpoints.
  - Ported `pi-antigravity` v0.15.0 public → runtime model & integer `thinkingBudget` routing (`gemini-3.8-flash` / `3.7` / `3.6` / `3.5` / `3.1-pro` / `claude-sonnet-4-6` / `claude-opus-4-6` / `gpt-oss-120b`), bounded capacity backoff on load-shaped `429`/`503`, `businessaicode*.googleapis.com` enterprise hosts, and actionable `403 VALIDATION_REQUIRED` errors.
