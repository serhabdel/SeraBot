# Release 0.74.8

Patch batch on 0.74: the member's **Library** — one cross-chat collection of
every file a bot made plus the member's own uploads, in a desktop page with
its own tabs, mirrored to the member's OneDrive by default.

## Server: `/v1/library/*` (`crates/serabot/src/library.rs`, `api/library.rs`)

- `GET /v1/library` lists the acting user's library, newest first: `generated`
  items (every artifact recorded in any of their conversations, with the
  conversation title) and `uploaded` items (the member's own uploads, recorded
  under the virtual scope `library` in `conversation_artifacts` — still
  UID-scoped, and served by the existing per-user artifact
  download/preview endpoints). The reply also carries the OneDrive mirror
  status for the account (`mirror`, `folder`, `connected`).
- `POST /v1/library/upload` takes a raw body with an `X-Filename` header (same
  pattern as chat attachments, up to the 100 MB artifact ceiling), applies the
  blocklist to the filename, saves it into the per-user artifact store,
  records it under the library scope, audits `library.upload`, and mirrors it
  best-effort to the member's own OneDrive. A mirror failure never fails the
  upload; the status string is returned.
- `[library]` config (default on): `enabled`, `onedrive_mirror`,
  `onedrive_folder` (default `SeraBot Library` under the OneDrive base folder).
- `social_extract` downloads (`download_images: true`) are mirrored through
  the same `LibraryMirror` (0.74.8), so extracted social images land in the
  library folder too; the tool output reports the mirror outcome.
- Store: `list_library_items(user_id, limit)` joins `conversation_artifacts`
  with `conversations` for titles, scoped to the acting user;
  `ArtifactStore::size(hash)` for listing sizes.

## Desktop: Library page (`ui_library.rs`, 0.74.8)

- New **Library** entry in the nav rail (Data group) opening a page with its
  own tabs (All / Made by bots / Uploaded / Media / Documents), search, date
  chips, sort, date-grouped list, and an item dialog (Open, Show in Explorer,
  Save to Downloads, Go to chat for bot-made files).
- **Upload** button (native picker, any file type up to 100 MB, up to 8 at a
  time) posting to `/v1/library/upload`; the header shows the OneDrive mirror
  status (`SeraBot Library` when connected, "not connected"/"off" otherwise).
- Client: `library()` / `library_upload()` in `api.rs` with
  `LibraryStatus` / `LibraryUploadReply` / `LibraryOnedriveStatus` models,
  reusing the `bot_library` view rules.

## Housekeeping

- Workspace version 0.74.8 (`Cargo.toml`), mobile `0.74.8+82`
  (`apps/serabot_mobile/pubspec.yaml`), `tasks/todo.md`, `README.md`,
  `AGENTS.md`.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo check -p serabot-desktop --features ui-check
  --all-targets`, `cargo test --workspace --locked` — clean (CI).
- New tests: `library_api.rs` (upload validation 400/413, upload lands in
  store + list + audit, per-user scoping incl. artifact download 404 for
  another member, generated items with conversation title, mirror status
  string without OneDrive configured) and `library.rs` mirror unit tests;
  store-level `list_library_items` scoping test.
