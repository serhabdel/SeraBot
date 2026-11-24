# Release 0.74.9

Patch batch on 0.74: the **Library big upgrade** — image cards with real
thumbnails and a full image viewer, easy opening of every file, and native
in-app previews for PDF, PowerPoint and Excel, with "Open with default app"
when the preview is not enough. Built on the 0.74.8 Library
(`GET /v1/library`, upload, OneDrive mirror); no new scopes, no new
dependencies.

## Server: thumbnails + document previews (`api/library.rs`, `pptx_ops.rs`)

- `GET /v1/library/thumbnail?hash=<sha256>&w=320` — a real thumbnail route
  (the artifact endpoints had none: download serves original bytes as an
  attachment, preview returns base64 in JSON with no size param). UID-scoped
  via the same `user_owns_artifact` gate as downloads (foreign hash → 404,
  malformed hash → 400, non-image → 400). `w` defaults to 320, clamps to
  16–1024, never upscales. PNG/JPEG/WebP are downscaled aspect-preserving
  (Triangle filter; JPEG q82 when opaque, PNG when alpha — re-encoding also
  drops EXIF/GPS); GIF/BMP/SVG are served as-is for the client to scale.
  Source cap 50 MiB. Response: `cache-control: private, max-age=86400`,
  `x-content-type-options: nosniff`, `content-disposition: inline`.
- `GET /v1/library/preview-doc?hash=<sha256>&kind=pptx|xlsx` — bounded,
  read-only document previews, UID-scoped the same way (404 when `[library]`
  is disabled, foreign hash → 404, kind/file mismatch → 400).
  - PPTX: `{"kind":"pptx","slide_count":<complete>,"slides":[{"slide":1,
    "title":"…","text":"…","truncated":false}],"truncated":false}` — max 50
    slides, 2000 chars of shape/table/notes text per slide, 100 000 chars
    total. Parsing reuses `pptx_ops::open()` preflight (32 MiB, 500 slides,
    no macros/encryption) and the new `pptx_ops::slide_preview()` helper.
  - XLSX: `{"kind":"xlsx","sheet_count":<complete>,"sheets":[{"name":"…",
    "range":"A1:B2","rows":[[…]],"truncated":false}],"truncated":false}` —
    max 5 sheets, 20 rows × 20 cols per sheet, cell strings capped at 200
    chars, built on `workbooks::import()` + `read_range()`; formulas are
    never evaluated (cached value only).
  - Extracted text is returned only as JSON string values, never executed or
    rendered as HTML — the same rule as the artifact preview. No blocklist
    scan: this is a read-only preview of the owner's own artifact.
- No new dependencies (`image` 0.25 was already in the tree).

## Desktop: Library page (`ui_library.rs`, `api.rs`, `system.css`)

- **Grid/List toggle** in the toolbar; the existing date-grouped list is the
  List option. Grid keeps the date-group headings (one grid per group).
  Image cards show a real thumbnail, filename, size, date, Made-by-bots /
  Uploaded, bot/chat, and type badge; non-images get a type-badge card.
  Thumbnails are fetched through the authenticated `ApiClient` as data URLs
  (a bare `<img src>` cannot carry the bearer token): `thumbnail?w=320`
  first, falling back to the artifact preview on 404; only the first 48
  images (≤25 MB each) are fetched, failures are remembered so cards never
  retry in a loop. Each card has an explicit **Open** button and **Details**.
- **Image viewer** — clicking an image card (or "View full size") opens a
  full overlay: large image, zoom out / % / in / reset (25–400%), Previous /
  Next through the currently filtered images (wraps), Save to Downloads,
  Open with default app, Go to chat. Keyboard: Esc closes, ←/→ step,
  +/−/0 zoom.
- **In-app document previews** in the item dialog (auto-loads on open):
  PDF reuses the existing viewer (`session.open_preview_dock(hash)` —
  PreviewDock with page nav + zoom); PPTX renders slide count, titles and
  text (first 50 slides); XLSX renders sheet tabs + first rows as a table
  (first 50 rows × 20 cols) with a truncation note; DOCX/MD/TXT/CSV render
  extracted text. All extracted text is rendered only as Dioxus text nodes
  (auto-escaped, never HTML) and bounded client-side.
- **Open with default app** — the dialog's primary button for every item,
  plus Show in Explorer and Save to Downloads. Reuses the existing flow
  (`session.open_artifact` → download via the existing save path → the OS
  opener; on Windows `explorer <path>` launches the file's default app).
  No shell interpolation; the path is a single already-validated local
  artifact file. Library ids are artifact hashes for generated and uploaded
  items, so the artifact routes work for all items.

## Mobile: per-bot Library screen (`bot_library_screen.dart`)

- No separate main Library screen exists on mobile, so the upgrade lands on
  the per-bot `BotLibraryScreen` (the only Library screen).
- **Image cards** — thumbnails are `Image.memory` (`cacheWidth: 160`) from
  the authenticated artifact preview (`image_b64`), falling back to the
  full download; failures show the plain image icon, never a broken tile.
  Tiles show filename, size, date, and a type badge. No thumbnail URL is
  used — a bearer token cannot go in an `Image.network` URL.
- **Image viewer** — full-screen `InteractiveViewer` (0.5–6× pinch zoom),
  prev/next through the filtered images with "N of M", share/save, close.
- **Document preview** — PPTX/XLSX open a sheet over
  `GET /v1/library/preview-doc` (slides as title/text cards, sheets as a
  sheet-picker + table of the first 50 rows). On 404 (server without the
  route) or an empty preview it shows "Preview not available — open the file
  instead." with Download/share — never a crash. PDF reuses the existing
  `ArtifactPreviewPane`.
- **Open with default app** — not a direct open on mobile yet:
  `url_launcher`/`open_file` are not in `pubspec`, so download/share goes
  through the existing `DeviceActions.shareFile` (temp file + system share
  sheet), which is how the user saves or opens a file in another app. Adding
  `open_file` to `pubspec` and wiring it in `_shareItem` would make it a
  true direct open.

## Housekeeping

- Workspace version 0.74.9 (`Cargo.toml`), mobile `0.74.9+83`
  (`apps/serabot_mobile/pubspec.yaml`), `Cargo.lock` synced,
  `tasks/todo.md`, `README.md`, `AGENTS.md`.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo check --workspace --all-targets --locked`,
  `cargo check -p serabot-desktop --features ui-check --locked`,
  `cargo test --workspace --locked` — clean locally (see PR checks for the
  full matrix; Windows-target clippy for `serabot-desktop` also run — the
  0.74.3 miss).
- `flutter analyze` — no issues; `flutter test` — 227 passed.
- New tests: server `library_api.rs` (5 new: pptx titles/text, xlsx sheets/
  first rows, foreign-hash 404 + bad-request 400s, caps, thumbnail
  downscale + UID scoping) and desktop `api::tests::
  library_doc_preview_parses_and_bounds`; mobile `bot_library_test.dart`,
  `api_client_test.dart`, `bot_library_screen_test.dart`.
- Not verified live: Windows WebView2 rendering (grid, viewer zoom/scroll,
  PreviewDock from the Library page), live end-to-end calls against a
  running 0.74.9 server, real PDF/PPTX/XLSX files on device, mobile APK
  build (no Android SDK in this environment — CI covers it).
