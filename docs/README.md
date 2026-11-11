# SeraBot documentation

SeraBot is a single-operator, self-hosted Rust agent. These documents describe
the current implementation and operational procedures. Examples are synthetic;
never commit real credentials, private policy terms, business documents, or
artifacts.

| Document | Purpose |
| --- | --- |
| [Model routing](model-routing.md) | Configure providers/models, consent, selection, and pinned turns. |
| [Antigravity](antigravity.md) | One-time Google PKCE login and Cloud Code catalog/chat. |
| [Orchestration](orchestration.md) | Orchestrator bot, scope ids, bounded handoffs, fleet rules. |
| [Audio](audio.md) | Mistral Voxtral transcription/understanding, inbox tools, Telegram intake. |
| [Owner API](api.md) | Authenticated HTTP/SSE contract used by the owner and clients. |
| [Desktop client](desktop-client.md) | Native Windows client build, launch, and current limitations. |
| [Deployment](deployment.md) | Ubuntu 24.04 release build and additive install on OVH. |
| [Firecrawl](firecrawl.md) | Optional v2 scrape/map/search/crawl tools and configuration. |
| [Browser sessions](browser.md) | Loopback CDP browsing vs BrowserOS neo / Firecrawl. |
| [Connectors](connectors.md) | Configure-later Google, Microsoft, and Notion OAuth; native AgentMail. |
| [Mobile client](mobile-client.md) | Flutter Android owner console. |
| [CI and secrets](ci-and-secrets.md) | GitHub Actions without credential leaks. |
| [Crafting Apps adoption](craft-adoption.md) | Storytold craft repos: take `deckcraft-pptx` for text-only generated decks and bounded import/edit of owner decks; skip the editors, MCP servers, and egui shells. |
| [ElevenLabs](elevenlabs.md) | Speech, STT, sound effects, and image/video flow tools. |
| [Owner wake checklist](owner-wake.md) | Morning status: `main` tip, OVH binary for `27b1018`, configure-later, smoke commands. |
| [Knowledge packs](knowledge-packs.md) | Import the owner's own accounting guides into the comptable bot's document inbox. |
| [Multi-user spec](specs/multi-user-reconstruction.md) | Owner-approved and implemented: small-circle multi-user — OIDC sign-in, UID-scoped data, per-user bots, credentials, and connectors. |
| [Big Wave (0.29–0.31)](specs/big-wave.md) | Visibility, reliability, and deep ops waves — Workflows, traces, per-user schedules, vault/browser polish; checklist in `tasks/big-wave.md`. |
| [Release 0.33.0](release-0.33.0.md) | Server retries and on-demand digest; desktop and Android client polish. |
| [Release 0.32.0](release-0.32.0.md) | Server stability and security (feed-URL visibility, session expiry, retries) and Android parity with desktop polish. |
| [Release 0.31.0](release-0.31.0.md) | Wave C gate: browser/vault polish, admin reliability counts, Flutter Workflows/traces. |

## Bot roster

The default fleet seeds twelve bots per user from `bots/*.toml` and
`prompts/*.txt`. `main` is the only orchestrator and delegates to the others.
Each bot has a default picture (`main` uses the brand icon) that an upload
overrides.

| Bot | Role | Focus | Key tools |
| --- | --- | --- | --- |
| main | orchestrator | Front door; routes requests and delegates | delegate, rag_search, deck and office tools |
| analyst | specialist | Documents, transcripts, spreadsheets | xlsx and docx tools, document_index |
| maker | specialist | Decks, Word and Excel deliverables | deck_build, docx_build, onedrive (approval) |
| watcher | specialist | Monitored sources, deadlines, briefs | watch_now, fetch_url |
| legal | specialist | Curated legal corpus (disabled until curated) | rag_search |
| comptable | specialist | Moroccan accounting, tax, payroll | rag_search, xlsx tools, ma-tax-2026, pcm-cgnc |
| auditor | specialist | Checklists, controls, reconciliations | rag_search, xlsx tools |
| marketer | specialist | Campaigns, copy, social posts, SEO briefs | deck_build, docx_build, web search |
| markets | specialist | Market and stock research, read-only | web search, xlsx tools |
| courier | specialist | Telegram drafts; sends files only with approval | telegram_send_document (approval), composio (approval) |
| whatsapp | specialist | WhatsApp drafts and approved sends | composio tools (every execute needs approval) |
| inbox | specialist | Email triage and drafts; never sends | gmail and outlook list tools, composio (approval) |

Bots that send or delete anything through an external account keep approval
on: `telegram_send_document`, `composio_execute` (for the messaging bots), and
`onedrive_upload`. Every bot gets `conversation_library` (find artifact hashes,
media results and local files itself) and `telegram_send_document` (0.27).
Only `main` has the `delegate` tool, and it may fan out up to 8 handoffs in one
turn to run parallel subagents.

Related sources of truth:

- `AGENTS.md` — product boundary, security invariants, build commands.
- `serabot.toml` / `deploy/serabot.production.toml.example` — synthetic configuration examples.
- `the-agent-project-strategic-dossier-v1-oct-2026.md` — product design and decision record.
