# Release 0.12.0 — multi-user, office tools, voice and OCR

## Headline

SeraBot becomes a small-circle, invitation-only multi-user agent, and gains the
office tooling plus voice replies and document OCR.

## Multi-user

- **Sign-in** is a thin Microsoft OIDC passthrough (PKCE, JWKS-verified
  id_token). No passwords are stored. Auth is off by default and every route
  fails closed while it is off.
- **Invitation only**: the admin mints single-use codes. The operator's
  existing API and CLI tokens keep working as the admin's sessions — nothing is
  re-issued.
- **Everything user-owned is UID-scoped**: conversations, tasks, approvals,
  memory, documents, bots, skills, credentials. The store layer has no unscoped
  path to user data.
- **Per-user bot fleets** seeded from the shipped templates, per-user skills,
  per-user memory.
- **Envelope-encrypted credentials**: a master key wraps a per-user data key
  which seals provider tokens with ChaCha20-Poly1305. Per-user OneDrive and
  per-user AI provider credentials, with the operator's existing secrets
  imported losslessly.
- **Per-user Telegram linking** via a single-use `/start` code.
- **Per-user authority**: members approve their own tool calls; the admin keeps
  audit visibility. Daily task quotas and per-user rate limits.

## Office tools

- DOCX and PPTX to PDF export.
- Telegram file intake (DOCX/XLSX/PPTX/PDF/CSV/TXT/MD) into a per-user inbox.
- Firecrawl structured extraction, approval-gated.
- XLSX workbook work and the conversation artifact library.

## Voice and OCR

- **OCR fallback** for scanned PDFs and images via `mistral-ocr-latest`, only
  when the built-in extractor finds less than `ocr.min_text_chars`.
- **Voice replies** via `voxtral-mini-tts-2603`: `/voice` and `/text` in
  Telegram, or the Voice page in either console. Every failure falls back to
  text so content is never lost.
- **Settings surface** in the desktop and mobile consoles: readiness chips,
  model and voice ids, OCR toggle, default reply mode.

## Upgrade notes

1. **Back up** `private/master.key` with the database once you enable per-user
   credentials. Without it, member credentials cannot be decrypted.
2. **Reseed the fleet** to pick up the new office tools:
   `serabot fleet reseed --dry-run` then `serabot fleet reseed`.
   The database is authoritative after seeding, so a binary upgrade never
   rewrites a fleet on its own.
3. **Sign-in** stays off until you follow `docs/deployment.md` (register the
   Azure public client, store `auth_client_id`, set `[auth] enabled = true`).
4. **OCR and voice** stay off until `[ocr].enabled` / a `voice_id` are set.
   `GET /v1/settings/voice` says exactly what is missing.

## Verification

Windows and Linux: `fmt` clean, `clippy -D warnings` clean, full workspace test
suite green, release build green. Mobile: `flutter analyze` clean and tests
green. Every provider test uses synthetic fixtures; no live account was
contacted.
