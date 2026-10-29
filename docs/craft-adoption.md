# Crafting Apps — adoption notes

Evaluated 2026-10-29 against the SeraBot pipeline (bounded deck spec, deterministic PDF, cited document ingest, gateway audio). Sources are the public GitHub repositories. Private `github.com/serhabdel/*` mirrors were not required.

None of these applications should be vendored. No crate from these repos is on crates.io as a standalone library. Each workspace pins Rust 1.90 or 1.95 and git revisions of `storytold/craft-libs` (and, for the UI, a forked egui). SeraBot is Rust 1.85, MIT, `printpdf` 0.7, `lopdf` 0.34, desktop `eframe` 0.31.

## License (all five)

Code grant: **MIT OR Apache-2.0**, at the user's option. Copyright (c) 2026 ArtCraft Team and that app's contributors. `Cargo.toml` says `license = "MIT OR Apache-2.0"`. GitHub's detected SPDX id is `Apache-2.0` because `LICENSE-APACHE` exists beside `LICENSE-MIT`.

Choosing MIT matches this workspace. Keep the copyright and permission notice with any copied source. Apache's `NOTICE` duties apply only if Apache is the license chosen.

Do not copy:

- `docs/brand/` — ArtCraft name, wordmark, and logos. Trademark terms in `docs/brand/LICENSE-brand.txt`. Forks must remove them.
- Bundled fonts (SIL OFL 1.1, including reserved font names) and other assets listed in each repo's `NOTICE`, `ATTRIBUTION.md`, or `ASSETS.md`.
- PdfCraft `vendor/` patches of hayro and lopdf. Those trees carry their own notices (hayro MIT OR Apache-2.0; Foxit standard-14 fonts BSD-3-Clause; Adobe CMap resources BSD-3-Clause).

The application source is not proprietary. The blocker on a wholesale copy is the asset and trademark split, the patched dependency tree, and the fit with SeraBot's security rules — not an unclear code license.

## Ranked recommendations

### 1. PdfCraft — adapt

https://github.com/storytold/pdfcraft

PDF reader and editor (version 0.15.0). Page rasterization goes through **hayro** (`crates/render/src/lib.rs` still describes that as a bootstrap). Inspection still uses lopdf, with a path patch under `vendor/` (`vendor` is excluded from the Cargo workspace).

Reuse as **patterns**, not as a subtree:

- Reading-order text in `pdfcraft-render` (`text`, `reading`).
- Page structure edits in `pdfcraft-organize` (rotate, delete, move, insert).

If ingest quality is the gap, add upstream **hayro** (or plan a separate `lopdf` bump). Do not import PdfCraft's patched copies.

Leave out: `pdfcraft-js` (Boa, Acrobat JavaScript), XFA, signing, `pdfcraft-automation` (MCP), and `pdfcraft-ocr` (`ocrs` / `rten` models). SeraBot rejects scanned PDFs on purpose (`documents.rs`) and does not execute document scripts. Deck PDFs stay on `printpdf` so output cannot gain images, links, or embedded files (`render.rs`).

### 2. DesignCraft — adapt later

https://github.com/storytold/designcraft

Page-layout studio (version 0.14.0). Paragraph composition is Knuth–Plass (`designcraft-compose`). PDF export is krilla via `designcraft-pdf` and embeds font subsets. PPTX export (`designcraft-pptx`) is bound to `designcraft-doc`, not a general slide writer. The app exposes its own MCP server.

That model conflicts with the current deck: Helvetica only (`deck.rs`), WinAnsi vectors, no images or attachments, one gateway. Treat the composer and the krilla export as a **design reference** if the owner later allows rich pages. Do not import code, fonts, or the MCP server before that policy change.

### 3. FilmCraft — skip

https://github.com/storytold/filmcraft

Nonlinear video editor (version 0.14.0), including a pure-Rust H.264 decoder. Speech-to-text is optional Candle Whisper with a model downloader (`filmcraft-speech`). TTS is a formant voice plus optional Kokoro (`filmcraft-tts`).

SeraBot already transcribes and synthesizes through Mistral Voxtral and ElevenLabs, behind approval and the blocklist. A local model download path is out of scope. Codec and timeline crates do not serve deck or PDF rendering.

### 4. EffectCraft — skip

https://github.com/storytold/effectcraft

Motion graphics and compositing (version 0.18.0). `effectcraft-pdf` parses PDF, Illustrator, and a PostScript subset into an SVG render tree for footage. `effectcraft-render` is a CPU compositor. Video generation, when SeraBot does it, is the approved ElevenLabs flow, not a local compositor.

### 5. SoundCraft — skip

https://github.com/storytold/soundcraft

Digital audio workstation described as a clean-room Pro Tools reimplementation (version 0.14.0, Rust 1.90). `soundcraft-audio-io` (WAV/AIFF and symphonia decode) is the only crate near SeraBot's inbox audio, and the pipeline does not decode locally. `soundcraft-vst3-host` (and the CLAP/AU hosts) load third-party plugins with `libloading` and an unsafe FFI crate. Plugin execution does not belong behind the tool allowlist. The Pro Tools name is not covered by the MIT/Apache grant.

## What not to do

- Do not add these apps, their egui shells, or their MCP servers to the workspace.
- Do not copy `docs/brand/`, fonts, icons, or `vendor/`.
- Do not run PDF JavaScript or host audio plugins from the agent.
- Do not stage OCR, Whisper, or Kokoro weights in this repository.
