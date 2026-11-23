# Release 0.74.6

Patch batch on 0.74: a new **social media extractor** capability, granted to
every bot.

## `social_extract` tool (`crates/serabot/src/social_extract.rs`, 0.74.6)

- Takes a public post `url` (Instagram, TikTok, X/Twitter, LinkedIn, Facebook,
  YouTube, Pinterest, Reddit, Threads, Tumblr, Vimeo, …) and returns
  structured data: `platform`, `url`, `title`, `author`, `caption`,
  `published`, `metrics` (likes/comments/shares/views from JSON-LD
  `interactionStatistic`), `images[]`, `videos[]`.
- Extraction is layered and best-effort: OpenGraph/Twitter meta cards, JSON-LD
  (`VideoObject` / `SocialMediaPosting` / `ImageObject` / `ImageGallery`, incl.
  `@graph`), then a deduped, absolutized scan of `<img src/srcset>` (largest
  srcset candidate wins) and `<video poster/src>`.
- `download_images: true` saves up to `max_images` (1–20, default 8) images
  into the acting member's artifact store — SSRF-safe GET per image, `image/*`
  only, ≤ 20 MiB per image, ≤ 50 MiB per call — and records each artifact in
  the conversation's UID-scoped Library (`record_conversation_artifact`).
- All fetches go through `SafeFetcher` (no IP literals, no private ranges, no
  userinfo, redirects revalidated, bounded bodies). A dedicated fetcher
  without the watcher host allowlist is registered in `main.rs`; `fetch.rs`
  gained `fetch_bytes`/`FetchedBytes` for raw media bodies. Read-only tool,
  `external_effect: false`; page content is marked untrusted in the output.

## Fleet-wide grant

- `social_extract` added to `SHARED_TOOLS` (`collaboration.rs`) and
  `known_tool_names` (`tools.rs`), registered in `main.rs` — every bot gets it
  on every turn, no per-bot toml change needed.

## Skill

- New `skills/social-extract/SKILL.md` (registered in `shipped.rs`, fleet
  skills now 39): workflows for metadata-only extraction and extract+download,
  caps, and the untrusted-content/SSRF rules. `docs/skills.md` row added.

## Housekeeping

- Workspace version 0.74.6 (`Cargo.toml`), mobile `0.74.6+80`
  (`apps/serabot_mobile/pubspec.yaml`), `tasks/todo.md`, `README.md`,
  `AGENTS.md`.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy -p serabot -p serabot-desktop
  --all-targets -- -D warnings` (incl. `--features serabot-desktop/ui-check`),
  `cargo check --all-targets` — clean.
- New `social_extract` tests: platform detection, meta/JSON-LD/srcset parsing,
  filename sanitization, and an end-to-end wiremock test (post page + media)
  asserting extraction, the 20-image cap, artifact saving, and the
  no-download default. Full `cargo test --workspace` runs in CI.
- Not verified live: real platform pages (Instagram/X often require login for
  full media) — extraction is best-effort by design; owner to try real posts.
