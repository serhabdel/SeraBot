# SeraBot documentation

SeraBot is a single-operator, self-hosted Rust agent. These documents describe
the current implementation and operational procedures. Examples are synthetic;
never commit real credentials, private policy terms, business documents, or
artifacts.

| Document | Purpose |
| --- | --- |
| [Model routing](model-routing.md) | Configure providers/models, consent, selection, and pinned turns. |
| [Orchestration](orchestration.md) | Orchestrator bot, scope ids, bounded handoffs, fleet rules. |
| [Audio](audio.md) | Mistral Voxtral transcription/understanding, inbox tools, Telegram intake. |
| [Owner API](api.md) | Authenticated HTTP/SSE contract used by the owner and clients. |
| [Desktop client](desktop-client.md) | Native Windows client build, launch, and current limitations. |
| [Deployment](deployment.md) | Ubuntu 24.04 release build and additive install on OVH. |
| [Skills](skills.md) | On-demand SKILL.md packs, tools, and operator scaffolding. |
| [Firecrawl](firecrawl.md) | Optional v2 scrape/map/search/crawl tools and configuration. |
| [Crafting Apps adoption](craft-adoption.md) | Storytold craft repos: adapt PdfCraft patterns only; skip film/audio/video editors. |
| [Browser sessions](browser.md) | On-demand loopback CDP browsing vs BrowserOS neo / Firecrawl. |
| [Connectors](connectors.md) | Google, Microsoft, Notion scaffolds; native AgentMail tools. |
| [ElevenLabs](elevenlabs.md) | Speech, STT, sound effects, and image/video flow tools. |
| [Mobile client](mobile-client.md) | Flutter Android owner console build and VPS connection. |
| [CI and secrets](ci-and-secrets.md) | Public GitHub Actions; client URL/token handling without leaks. |
| [Owner wake checklist](owner-wake.md) | Morning verification: Telegram, clients, OneDrive, secrets, OVH binary notes. |

Related sources of truth:

- `AGENTS.md` — product boundary, security invariants, build commands.
- `serabot.toml` / `deploy/serabot.production.toml.example` — synthetic configuration examples.
- `the-agent-project-strategic-dossier-v1-oct-2026.md` — product design and decision record.
