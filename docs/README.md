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

Related sources of truth:

- `AGENTS.md` — product boundary, security invariants, build commands.
- `serabot.toml` / `deploy/serabot.production.toml.example` — synthetic configuration examples.
- `the-agent-project-strategic-dossier-v1-oct-2026.md` — product design and decision record.
