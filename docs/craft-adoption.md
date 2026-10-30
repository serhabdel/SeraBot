# Crafting Apps — adoption notes

Checked 2026-10-30 against the SeraBot pipeline (bounded deck spec, deterministic
PDF, cited document ingest, gateway audio). Sources are the public repositories
under the `storytold` GitHub organisation (display name **ArtCraft**) and the
crates.io registry API. No private mirror was needed.

SeraBot is Rust 1.95, MIT, `printpdf` 0.7, `lopdf` 0.34, desktop `eframe` 0.31.
It takes exactly one thing from this organisation: the `deckcraft-pptx` crate,
used to write generated decks and to round-trip owner-supplied ones.

## The organisation today

| Repository | Workspace version | MSRV | Licence | What it is |
| --- | --- | --- | --- | --- |
| `storytold/deckcraft` | 0.12.0 | 1.95 | MIT OR Apache-2.0 | PowerPoint reimplementation: model, geometry, colour, text, PDF, PPTX, native format, engine, egui shell, MCP server, H.264/HEVC/VP9/AV1 codecs |
| `storytold/wordcraft` | 0.12.0 | 1.95 | MIT OR Apache-2.0 | Word reimplementation: doc model, layout, DOCX/DOCBIN, render, PDF, proofing, egui shell, MCP server |
| `storytold/gridcraft` | 0.13.0 | 1.95 | MIT OR Apache-2.0 | Excel reimplementation: model, formula, calc, XLSX, charts, PDF, egui shell, MCP server |
| `storytold/craft-libs` | 0.1.0 | 1.95 | MIT OR Apache-2.0 | Shared Rust libraries (UI, print, settings, ICC colour). Excludes `vendor/krilla` and the `vendor/hayro*` trees; pins `krilla = 0.8.2`, `hayro = 0.7.1` |
| `storytold/craft-ai-integrations` | 0.1.0 | 1.90 | MIT OR Apache-2.0 | Provider connections, keys, client, and egui widgets. Patches the whole `egui` family to a `storytold/egui` fork revision |
| `storytold/artcraft-services` | — | — | **none** | Backend monorepo: actix-web HTTP service, workers, MySQL, Stripe, provider clients, TypeScript frontends |

The apps consume `craft-libs` by git revision, not by version, and the UI stack
is a forked `egui`. That is a different dependency universe from SeraBot's
frozen `Cargo.lock`.

## What is actually on crates.io

Only the `deckcraft-*` family is published: `deckcraft-format`, `-color`,
`-geom`, `-model`, `-fonts`, `-text`, `-render`, `-anim`, `-media`, `-pdf`, and
`-pptx`, all at **0.6.1**, published 2026-10-24/25 by the **`aphrody-dev`**
account. None of them carries a `repository` field, so the registry does not
link a published crate back to this repository, and 0.6.1 is well behind the
0.12.0 workspace. Treat the published source as the authority for what we
compile, not the repository HEAD.

`deckcraft-pptx` 0.6.1 depends on `deckcraft-model`, `deckcraft-geom`,
`deckcraft-color` (all 0.6.1), `log` 0.4, `quick-xml` 0.38, `thiserror` 2, and
`zip` 8.6 with `deflate` only. `deckcraft-model` adds `serde`, `serde_json`,
`thiserror`, `unicode-segmentation`; `deckcraft-geom` adds `kurbo` 0.13;
`deckcraft-color` adds `serde`. All of it resolves in SeraBot's lockfile.

Two registry traps:

- `craft-ui` and `craft-cms` 1.4.7 are published by the same account but point
  at `aphrody-labs/craft-libs`, a mirror whose own workspace declares 0.1.0 and
  whose `repository` field still says `storytold/craft-libs`. The published
  numbers match neither repository. Do not substitute them for the pinned git
  revision.
- The `wordcraft`, `artcraft`, and `unicraft` names on crates.io belong to an
  unrelated publisher. They are not these projects.

## Licence position

The five licensed repositories grant **MIT OR Apache-2.0**, at the user's
option, copyright (c) 2026 ArtCraft Team and that app's contributors. Choosing
MIT matches this workspace; keep the copyright and permission notice with any
copied source. Apache's `NOTICE` duties apply only if Apache is chosen.

`artcraft-services` has **no `LICENSE`, `LICENSE-MIT`, `LICENSE-APACHE`, or
`NOTICE` file**, GitHub detects `NOASSERTION`, and its `[workspace.package]`
sets `publish = false` (its member crates repeat it). It is an unpublished,
unlicensed product backend. Read it for architecture only; copy nothing.

Do not copy, from any of them:

- `docs/brand/` — ArtCraft name, wordmark, logos. Trademark terms live in
  `docs/brand/LICENSE-brand.txt`; forks must remove them.
- Bundled fonts (SIL OFL 1.1, including reserved font names) and assets listed
  in each repo's `NOTICE`, `ATTRIBUTION.md`, or `ASSETS.md`.
- `craft-libs/vendor/` — patched krilla and hayro trees with their own notices
  (krilla MIT OR Apache-2.0; Foxit standard-14 fonts BSD-3-Clause; Adobe CMap
  resources BSD-3-Clause).

## Decision: `deckcraft-pptx`, two bounded paths

`crates/serabot/src/deck_pptx.rs` maps a validated `DeckSpec` plus `Brand` to a
`deckcraft_model::Presentation` and calls `deckcraft_pptx::export`. The deck
tool stores the result through `ArtifactStore` beside the PDF, so approval binds
its hash exactly like the PDF's.

`crates/serabot/src/pptx_ops.rs` adds the other direction for a package the
owner supplied: `pptx_import` (inbox path or artifact hash), `pptx_inspect`
(read-only), and `pptx_edit` (artifact hash plus a bounded edit spec). It is the
only production caller of `deckcraft_pptx::import`, and `deck_build` cannot
reach it.

The split, enforced by code and tests:

- **Generated decks stay text-only.** `deck_build` writes text, autoshapes, and
  theme colours. No images, media parts, hyperlinks, embedded workbooks,
  attachments, charts, transitions, animations, or font bytes. The package test
  asserts the absence of `ppt/media/`, `ppt/embeddings/`, `ppt/charts/`,
  `ppt/fonts/`, `<p:pic`, `<a:blip`, `hlinkClick`, `TargetMode="External"`,
  `r:embed`, `r:link`, `oleObject`, and `<p:timing`.
- **Edited decks preserve images.** An imported deck's media parts are the
  owner's content, not model output, so `pptx_edit` writes them back. The round
  trip is asserted: build a deck with a synthetic PNG, export, import, edit the
  text, export again, and check the image part is byte-identical and the text
  changed.
- **Hyperlinks and external links are dropped.** `pptx_edit` clears every
  shape, run, note, master, and media link before export, and then refuses to
  store a package whose `.rels` parts still carry `TargetMode="External"`.
  Internal relationships (a slide jump, an embedded clip) are unaffected.
- **Import is bounded and validated.** 32 MiB, 500 slides, 4096 zip parts,
  128 MiB uncompressed, and 20 000 shapes. Password-protected packages and any
  package carrying a VBA project (or a macro-enabled content type) are rejected
  outright. Every edit index is range-checked before anything mutates, and the
  edit spec is `#[serde(deny_unknown_fields)]`.
- **One inbox path.** `ArtifactStore::ingest_inbox_file` is the single generic
  inbox → artifact function: relative path only, canonicalised inside the
  configured inbox, extension allowlist, size cap, and the caller's inspection
  runs before anything is stored. No tool accepts a model-chosen absolute path.
- **No font embedding.** Runs name the `Helvetica` family; no font bytes are
  passed, and `Brand::validate` accepts no other family.
- **One validation gate for generated decks.** `deck_pptx` calls
  `deck::validate` first, with the same page budget as `render::render_deck`.
  There is no second, weaker path.
- **No new network, MCP server, or gateway.** The crate is a pure reader and
  writer; the new tools are ordinary gateway tools with `external_effect: false`.

This supersedes the earlier note that DesignCraft's PPTX export was unusable
because it was bound to `designcraft-doc`. DeckCraft publishes a standalone
slide writer, and 0.6.1 is the version we compile.

## The rest of the family

- **PdfCraft** (`storytold/pdfcraft`) — adapt patterns only. Reading-order text
  and page-organise edits are useful references. Do not import its `vendor/`
  patches of hayro and lopdf; add upstream hayro if ingest quality becomes the
  gap. Leave out `pdfcraft-js` (Boa, Acrobat JavaScript), XFA, signing,
  `pdfcraft-automation` (MCP), and `pdfcraft-ocr` (`ocrs`/`rten` models).
  SeraBot rejects scanned PDFs on purpose and does not execute document scripts.
- **DesignCraft** (`storytold/designcraft`) — a page-layout studio whose
  composer is Knuth–Plass and whose PDF export is krilla with embedded font
  subsets. Useful as a design reference if the owner ever allows rich pages. Do
  not import the composer, the fonts, or its MCP server.
- **WordCraft** — DOCX/DOCBIN authoring with its own layout engine. Out of
  scope: SeraBot's document path is ingest and citation, not authoring.
- **GridCraft** — a spreadsheet engine with formula evaluation. SeraBot's
  `spreadsheets.rs` reads cells with provenance and never evaluates formulas;
  adopting GridCraft's calc would add an execution surface behind the tool
  allowlist for no deliverable gain.
- **FilmCraft, EffectCraft, SoundCraft** — skip. Local Whisper/Kokoro model
  downloads, a CPU compositor, and a VST3/CLAP/AU host (`libloading` plus unsafe
  FFI) all conflict with the gateway model. Speech stays on Mistral Voxtral and
  ElevenLabs behind approval and the blocklist.

## What not to do

- Do not vendor these repositories or add them to the workspace as path or git
  dependencies.
- Do not add their MCP servers or egui shells. SeraBot has one gateway and one
  owner console.
- Do not copy `docs/brand/`, fonts, icons, or `vendor/`.
- Do not copy anything from `artcraft-services`: no licence, `publish = false`.
- Do not run PDF JavaScript, evaluate spreadsheet formulas, or host audio
  plugins from the agent.
- Do not stage OCR, Whisper, or Kokoro weights in this repository.
- Do not call `deckcraft_pptx::import` outside `pptx_ops`, and never from
  `deck_build`'s path.
