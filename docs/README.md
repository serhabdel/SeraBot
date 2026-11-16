# SeraBot documentation

SeraBot is a small-circle, invitation-only multi-user self-hosted Rust agent. These documents describe
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
| [Release 0.37.0](release-0.37.0.md) | Bots finish the task instead of refusing; live step cards; typed tool results; page data for Tasks, Memory, Connectors, and Providers. |
| [Release 0.38.0](release-0.38.0.md) | Working chat switching and a steadier chat view; one Integrations screen; plugin tools for bots; safe links and richer markdown in chat. |
| [Release 0.39.0](release-0.39.0.md) | Per-user isolation & reliability fixes: scheduled/direct `dir:main` isolation, resilient scheduler loop, UID-scoped `kv` & `/v1/models` ownership, per-user monitoring sources, multi-user RAG vector recall, least-privilege `/v1/connectors`, Windows ACLs, and batched media jobs. |
| [Release 0.40.0](release-0.40.0.md) | Desktop UI/UX overhaul: chat & composer shortcuts, richer markdown & bounded cache, approvals & conversations polish, shell/search/banner fixes, Tasks/Workflows/Logs/Files/Jobs/Bots/Dock upgrades, and CSS/responsive refinements. |
| [Release 0.41.0](release-0.41.0.md) | Desktop integrations, providers, settings, usage, vault, browser, and memory UX overhaul: clear plugin/connector/OAuth/Composio states, provider draft validation & presets, Mem0/TurnLimits/WebSearch/LocalFolders/Vault/Browser/Memory/Sheets/RowMenu/Preview polish, and settings/usage CSS scaling. |
| [Release 0.42.0](release-0.42.0.md) | Desktop core resilience & server feature polish: fixed pending-gate refresh drops, `Cmd/Ctrl+K` active-screen search focus, standing-grant search/skeleton/confirm, SSE buffer cap & updater backup/restore; server brief/digest/analyst/watcher/spreadsheets/workbooks/documents hardening. |
| [Release 0.43.0](release-0.43.0.md) | Long conversation scroll & details auto-scroll, ElevenLabs OAuth 2.1-only & media handling fixes, Integrations overhaul & Marketplace base enabled (`RUNTIME_CALLS_PLUGINS = true`, builtin 10-plugin catalog, SVG brand marks), and artifact preview fixes. |
| [Release 0.44.0](release-0.44.0.md) | Rich visual preview dock across Image, PDF, PPTX, XLSX, and DOCX; expanded activity box & stop button; chat media card layout & pending job visibility; and step-more toggle polish. |
| [Release 0.45.0](release-0.45.0.md) | Wave 8 OAuth callback & connector test fixes, Proton Pass default & PAT fallback, Vault & Tasks full editing, and Visual Workflow Studio routine/workflow editing. |
| [Release 0.46.0](release-0.46.0.md) | Wave 9 Vault CLI auto-install, Tasks HTTP 415 + unified Calendar sync + layout fixes, OneDrive folder visibility & local-folder clarity, secret redaction + Composer Credential Guard, and ChatView search / create-task / starter chips. |
| [Release 0.47.0](release-0.47.0.md) | Wave 10 `jobfinder` specialist bot, smart conversation titles & `conversation.renamed` SSE, inline approvals (desktop/mobile/Telegram), 1-click Browser Vault autofill, Vault provider sync, compiled-in default bot avatars, and mobile Tasks/Chat/Preview upgrades. |
| [Release 0.48.0](release-0.48.0.md) | Logging hardening (noisy-crate caps, shared default filter, per-layer ring filter, durable owner-only `[logging] dir`), approval expiry check at decision time, and negative tests for jobs, workflows, preview, vault autofill and approvals. |
| [Release 0.49.0](release-0.49.0.md) | Provider accounts (several per member, sealed, write-only), member sign-in for Antigravity and Muse Code, native Anthropic and OpenCode backends, flag-gated Claude plan OAuth, single-flight refresh, bot routes with account/provider failover, and the desktop Providers, route editor and composer route chip. |
| [Release 0.50.0](release-0.50.0.md) | Orchestration at scale: fleet mutation path and `main` management tools, handoff ledger with auto-resume, `waiting_model` parking, context compaction, `needs_review` for unknown effects, per-user fairness, and the desktop virtualized feed, runs sidebar, composer queue and failure/wait UX. |
| [Events](events.md) | `turn.step` payload for desktop and Android step cards. |
| [Tool results](tool-results.md) | Audio, file, image, link, and table cards. |
| [Console pages](console-pages.md) | Tasks, Memory, Connectors, and Providers fields the next desktop pages read. |
| [Release 0.36.0](release-0.36.0.md) | Message box layout, generated media download fixes, Android build check. |
| [Release 0.35.0](release-0.35.0.md) | Several chats per bot, memory refresh, steadier chat view, bigger composer, Tasks fix. |
| [Release 0.34.0](release-0.34.0.md) | Desktop chat view and composer fixes; Tasks, Logs and Browser repairs; workflow management and page layout. |
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
Only `main` has the `delegate` tool, and it may fan out up to 16 handoffs in one
turn to run parallel subagents.

Related sources of truth:

- `AGENTS.md` — product boundary, security invariants, build commands.
- `serabot.toml` / `deploy/serabot.production.toml.example` — synthetic configuration examples.
- `the-agent-project-strategic-dossier-v1-oct-2026.md` — product design and decision record.
