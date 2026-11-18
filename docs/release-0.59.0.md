# Release 0.59.0

Projects end-to-end (server API, desktop workspace, and attachment-relocation
fix), real multimodal image vision across all four wire transports plus inline
Office excerpts, desktop drag-and-drop uploads, GitHub Copilot & OpenAI Codex
quota/transport parity, local Claude Code (ACP) `@`-file mentions and image
blocks, session-expiry warnings, conversation Markdown export, and WhatsApp
linked-device enabled by default. Synthetic fixtures only; nothing here was
verified against a live WhatsApp, Copilot, Codex, or remote LLM account.

## Projects workspace & relocation fix

- `projects.rs` & `attachments.rs`: `project_file` (`move`) now updates
  `chat_attachments.rel_path` in SQLite when moving an uploaded chat attachment
  into a project, and `GET /v1/attachments/{id}` serves relocated files under
  `{user_id}/projects/` so downloads and handoffs keep working.
- `api/projects.rs`: user-scoped `/v1/projects` routes (`GET/POST /v1/projects`,
  `GET /v1/projects/{slug}/files`, `POST /v1/projects/{slug}/upload`,
  `GET/DELETE /v1/projects/{slug}/files/{*path}`) with path-safety checks,
  policy scanning, and audit logging.
- Desktop `ProjectsView` (`Section::Projects` in the Data rail and Command
  Palette): create projects, browse files, upload, download, delete (with
  confirmation), and jump into chat with the project file's inbox path.

## Multimodal image vision & Office excerpts

- `domain.rs` & `attachments.rs`: `LlmImage` and `load_images_for_model`
  (capped at 4 images, 4 MiB each, 12 MiB total) feed image attachments into
  user turns (including handoff-inherited attachments) alongside an OCR tool
  hint.
- All four wire transports now serialize `LlmMessage.images`:
  - OpenAI-compatible (`/chat/completions` in `llm.rs`): `image_url` parts.
  - Anthropic (`wire/anthropic.rs`): base64 `image` source blocks.
  - OpenAI Responses (`wire/responses.rs`): `input_text` + `input_image` parts.
  - Antigravity / Cloud Code (`cloud_code.rs`): `inlineData` parts.
- `attachments::render_for_model` includes bounded inline text excerpts for
  `.docx`, `.xlsx`, and `.pptx` attachments.

## Desktop drag-and-drop, session expiry & Markdown export

- `ui_files.rs`: extended the chat bridge with `dragenter`/`dragover`/
  `dragleave`/`drop` listeners feeding the existing attachment upload queue
  with a `.chat.drop-active` visual drop target.
- `session.rs` & `ui/shell.rs`: `SessionExpiryBanner` warns within 48 hours of
  the persisted `session_expires_at` so members can re-sign-in before a 401;
  added a "Copy conversation as Markdown" button in `Topbar`.

## Copilot & Codex parity

- `api/provider_quota.rs`: added `CopilotQuota` (`/copilot_internal/user`) and
  `CodexQuota` (`/backend-api/wham/usage`) readers to `standard_readers()`.
- `llm.rs`: added `auto` to the Copilot model catalog, Responses-only model
  routing (`copilot_uses_responses`), and `reasoning_effort` forwarding.
- `wire/responses.rs`: captures `reasoning` items from Responses streams onto
  `ToolCall::reasoning_signature` (`resp:` prefix) and replays them ahead of
  assistant `function_call` items on subsequent tool rounds.

## Local Claude Code (ACP) ergonomics

- `acp/protocol.rs`, `acp/client.rs`, `acp/controller.rs`, `ui_claude_code.rs`:
  added `@`-file mention autocomplete scoped strictly to the allowed folder
  (skipping symlinks and build/VCS dirs) and base64 image prompt blocks
  (`PromptImage`, capped at 4 images / 4 MiB each). No widening of ACP
  capabilities (still desktop-only, no terminal, no "always" permissions).

## WhatsApp as a linked device (enabled by default)

- `[whatsapp_link] enabled` now defaults to `true` in `config.rs`,
  `serabot.toml`, and `deploy/serabot.production.toml.example` per owner
  approval (`enabled = false` still turns it off). Scope and safety boundaries
  are unchanged: spare number warning, one-to-one allowlist only, no groups,
  and approval-gated `whatsapp_link_send`.
