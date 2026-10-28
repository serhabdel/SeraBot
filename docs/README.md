# SeraBot documentation

SeraBot is a single-operator, self-hosted Rust agent. These documents describe
the current implementation and operational procedures. Examples are synthetic;
never commit real credentials, private policy terms, business documents, or
artifacts.

| Document | Purpose |
| --- | --- |
| [Model routing](model-routing.md) | Configure providers/models, consent, selection, and pinned turns. |
| [Owner API](api.md) | Authenticated HTTP/SSE contract used by the owner and clients. |
| [Desktop client](desktop-client.md) | Native Windows client build, launch, and current limitations. |
| [Deployment](deployment.md) | Ubuntu 24.04 release build and additive install on OVH. |

Related sources of truth:

- `AGENTS.md` — product boundary, security invariants, build commands.
- `serabot.toml` / `deploy/serabot.production.toml.example` — synthetic configuration examples.
- `the-agent-project-strategic-dossier-v1-oct-2026.md` — product design and decision record.
