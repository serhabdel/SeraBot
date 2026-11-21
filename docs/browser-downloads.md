# Browser downloads and PDF inspection

## Root causes

When a member asked a bot to download and inspect a PDF (or other inline-viewable document) in the managed Chromium browser, four separate issues caused the flow to fail:

1. **Built-in PDF viewer instead of `Browser.downloadWillBegin`:** Headless Chromium (`--headless=new`) renders `application/pdf` responses without `Content-Disposition: attachment` inside its built-in PDF viewer (`Page.getFrameTree` reports `frame.mimeType = "application/pdf"` and `isDownload = false`). Because no `Browser.downloadWillBegin` event fires, `browser_download` waited for the full timeout and failed.
2. **Popup (`target=_blank`) download race:** `ChromiumBackend::configure` previously sent `Browser.setDownloadBehavior { behavior: "deny" }` on every new target attachment (including `target=_blank` child tabs). That reset Chromium's browser-wide download policy mid-click, canceling downloads started by `target=_blank` links and leaving child-tab PDF viewer navigations unhandled.
3. **`ref`-only parameter:** `browser_download` accepted only an element `ref`, so a bot could not directly fetch an `href` discovered in a snapshot or download a PDF already open in the active tab.
4. **Opaque artifact output:** `save_artifact` returned `"Saved as artifact <name> (<n> bytes). The file content is not shown to you."` without reporting the MIME type, the `artifact_hash` in text, or which reading tool (`pdf_inspect`, `docx_import`, `xlsx_import`, `pptx_inspect`, `document_index`) to invoke next.

## New behaviour

### Download modes (`browser_download`)

`browser_download` now accepts **exactly one** of:

- `ref`: Clicks the referenced element while `allowAndName` is active (`Browser.setDownloadBehavior`), waits for `Browser.downloadWillBegin` / `Browser.downloadProgress`, and if no download begins within 2 seconds (or before the deadline) while the active tab (including a newly opened `target=_blank` tab) transitions to a document MIME/extension or the clicked element is a link to a document, falls back to fetching the resource inside the browser session via CDP `Network.loadNetworkResource` + `IO.read` / `IO.close`.
- `url`: Downloads a specific public `http`/`https` URL directly within the acting member's browser session (preserving cookies and authenticated headers, without CORS restrictions) via `Network.loadNetworkResource` + `IO.read` / `IO.close`.
- `current: true`: Saves the document currently open in the active tab (for example after navigating directly to a PDF viewer tab). Refuses `about:blank` and ordinary `text/html` pages (recommending `browser_download` with `ref`/`url` or `browser_pdf` instead).

`Browser.setDownloadBehavior { behavior: "deny" }` is set once at browser startup (`setup`) rather than per tab (`configure`), and is reliably restored after `click_download` finishes or fails. Note that `deny` blocks unsolicited attachment downloads while allowing inline PDF viewing (`isDownload: false`).

### Safeguards

- **SSRF / URL checks:** Every URL passed to `url`, resolved as the active viewer URL, or extracted from a clicked link is validated with `validate_navigation_url` and `validate_resolved` (public `http`/`https` only; loopback, link-local, RFC1918/ULA, and non-HTTP schemes are rejected). Note: Chromium's `Network.loadNetworkResource` handles HTTP redirects inside the browser network stack; Chrome continues to reject unsafe ports, and page-level `Fetch.requestPaused` does not intercept `loadNetworkResource` redirects.
- **Size cap (`MAX_FILE_BYTES = 25 MB`):** Both `Content-Length` pre-checks and incremental `IO.read` chunk totals enforce the 25 MB limit and close the CDP IO stream immediately on abort.
- **Filename & policy:** Filenames are derived from `Content-Disposition` (`filename*` or `filename`), decoded URL path segments, MIME type (`application/pdf` -> `.pdf`), or `%PDF` magic bytes, sanitized with `clean_download_name`, and checked against the outgoing/filename policy blocklist before saving.

### Artifact output & PDF preview

Both `browser_download` and `browser_pdf` now return:

- Saved filename, byte size, MIME type, and `artifact_hash` (in both text and structured `data`).
- Next-tool guidance (`pdf_inspect{artifact: "<hash>"}` and `document_index` for PDFs, `docx_import` for DOCX, `xlsx_import` for XLSX/CSV, `pptx_inspect` for PPTX).
- For PDFs with extractable text, a bounded (up to 1,500 characters) first-page text preview extracted with `extract_pdf_page_texts_bounded` (`lopdf`), scrubbed with `lease.redact`, policy-checked, and prefixed with the `UNTRUSTED BROWSER PAGE` warning banner.
