# On-demand browser sessions

SeraBot can start **short-lived, loopback-only** browser sessions for agent tasks.
Browser tools go through the gateway; page metadata is **untrusted** and blocklist-scanned.

## BrowserOS neo vs lightweight options

| Option | OVH 2 vCPU / 3.8 GB | Logged-in sites | Notes |
| --- | --- | --- | --- |
| **Firecrawl** | Excellent | No | Read-only scrape/map/search/crawl. |
| **`spawn_cdp`** | Good on demand | No | Headless Chromium; tear down to free RAM. |
| **`remote_cdp`** | Good | Owner-dependent | Attach to loopback CDP (SSH tunnel from BrowserOS neo / Chrome). |
| **[BrowserOS neo](https://github.com/browseros-ai/BrowserOS)** | Poor on VPS alone | Yes (owner PC) | Chromium fork + claw-server; run on workstation, not OVH. |
| Cloud browsers | Out of scope | Awkward | Datacenter IPs; conflicts with self-hosted boundary. |

**Recommendation:** Firecrawl for extraction; `remote_cdp` over SSH to neo on your PC for authenticated flows; `spawn_cdp` on VPS only for short logged-out tasks.

## Configuration

`[browser]` in `serabot.toml`: `spawn_cdp`, `remote_cdp`, or `browseros_neo`.

## Gateway tools

| Tool | Purpose |
| --- | --- |
| `browser_session_start` | Spawn or attach (approval required). |
| `browser_session_stop` | Kill spawned Chromium. |
| `browser_navigate` | Open HTTPS URL in new CDP target. |
| `browser_snapshot` | List targets (titles/URLs). |

See `docs/firecrawl.md` for full page text without a persistent browser.
