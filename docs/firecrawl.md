# Firecrawl integration

SeraBot can call the [Firecrawl](https://firecrawl.dev) v2 API for JavaScript-aware
scraping, site mapping, web search, and bounded crawls. All four operations are
gateway tools with `external_effect = true`, so they require owner approval
(unless standing consent applies) in addition to bot-level tool allowlists.

## Enable

1. Add `[firecrawl]` to `serabot.toml` (see the synthetic example in the repo root).
2. Store the API key in the age secret store:

   ```bat
   serabot secrets set firecrawl_api_key
   ```

3. Set `firecrawl.enabled = true`.
4. Allowlist tools on a bot, for example:

   ```toml
   tools = ["firecrawl_scrape", "firecrawl_map", "firecrawl_search", "firecrawl_crawl"]
   require_approval = ["firecrawl_crawl", "firecrawl_search"]
   ```

   `require_approval` is optional; every Firecrawl tool already triggers approval
   because of `external_effect`.

## Tools

| Tool | API | Notes |
| --- | --- | --- |
| `firecrawl_scrape` | `POST /v2/scrape` | Single URL → markdown |
| `firecrawl_map` | `POST /v2/map` | URL discovery (bounded link list) |
| `firecrawl_search` | `POST /v2/search` | Query + optional markdown bodies |
| `firecrawl_crawl` | `POST /v2/crawl` + poll | Waits until complete or timeout |

Markdown and metadata returned to the model are prefixed as **untrusted page
content** and are scanned by the blocklist like any other tool output.

## Limits

Configuration caps (`max_crawl_limit`, `max_search_limit`, `max_map_links`,
`max_markdown_chars`, `crawl_max_wait_secs`) bound cost and context size. Target
URLs must be `https://` hostnames (no IP literals, no embedded credentials).

## Verification

Unit tests use wiremock fixtures only. Live Firecrawl calls are not verified in CI.
