# Plugin marketplace

SeraBot plugins are described by a `plugin.json` file in `plugins/<slug>/`.
The catalog is read at boot and validated; a file that fails is reported and
skipped, and the others still load.

## Design credit and licensing

The layout (a catalog of plugin folders, each with one manifest, and apps with
connection and authentication options) was modelled on a public plugin catalog
whose license is noncommercial. No text, descriptions, or code from that catalog
are copied. Every descriptive field here was written for SeraBot, and the
schema is SeraBot's own.

## Schema

Schema version 1 is still loaded. Version 2 (v0.51) adds real sign-in and tool
policy. Both are validated at boot.

| Field | Meaning |
|-------|---------|
| `schemaVersion` | `1` or `2`. |
| `slug` | Lowercase letters, digits and dashes; must match the folder name. |
| `name`, `tagline`, `description`, `category` | Display text. |
| `iconUrl`, `websiteUrl`, `privacyPolicyUrl`, `termsUrl` | HTTPS URLs without credentials. |
| `version`, `minServerVersion` | Dotted numbers such as `1.0.0`. |
| `prompts` | Starter prompts (`id`, `text`). |
| `apps[]` | Each app has `id`, `name`, a `server` and `auth` options. |
| `apps[].server` | `transport: "http"` with `url`, or `transport: "stdio"` with `command` and `args`. |
| `apps[].auth[]` | v1: `kind` is `oauth-link` or `api-key`. v2: `oauth`, `api-key` or `none`; `oauth` takes `scopes`, an optional `clientId`, an optional `resourceMetadataUrl` and `usesCimd`. |
| `apps[].auth[].header` (v2) | Header name and `scheme` (`bearer` or raw) for an API key. `host`, `content-length` and `cookie` are refused. |
| `apps[].tools` (v2) | `allow`, `deny` and `readOnly` tool names. |
| `accounts.multiple` (v2) | Whether a member may sign in with several accounts. |
| `permissions.egress` | Bare host names the plugin may reach. |

## Runtime

Installed and enabled `http` apps expose per-user `plugin.<slug>.<tool>` tools
(`plugin_runtime.rs`). Every app of a plugin is used, not only the first. Calls
go over HTTPS to a public, pinned IP on the manifest's egress list, through the
gateway: approval, policy scan and audit apply.

- **Sign-in (MCP OAuth, `mcp_oauth.rs`).** Discovery follows RFC 9728, then RFC
  8414 or OIDC. The client identity is a client-id metadata document (CIMD) when
  the server supports it and the manifest asks for it, then dynamic client
  registration (RFC 7591), then a configured client id. PKCE S256 and an RFC
  8707 `resource` are used. Every endpoint must resolve to a public IP; replies
  are capped at 64 KiB and 20 seconds. `invalid_grant` drops the tokens and sets
  `needs_sign_in`; `invalid_client` also drops the registration; a network error
  or 5xx keeps everything. Uninstall makes a best-effort RFC 7009 revoke.
- **Accounts.** Per member, several per plugin when `accounts.multiple` is set
  (table `marketplace_accounts`, migration 0039). Tokens, expiry, scope and the
  client registration are sealed under `marketplace.<slug>.<account>.oauth` and
  are write-only. The state is `needs_setup`, `needs_sign_in`, `ready` or
  `unreachable`; a failure is recorded, not swallowed, and `main` says so
  ("Canva needs sign-in").
- **Bearer at call time.** The bearer is injected when the call is made, never
  placed in a prompt. A 401 refreshes once and retries.
- **Tool annotations and switches.** A tool whose MCP `readOnlyHint` is true skips
  approval but is still audited and scanned. Everything else asks, bound to the
  arguments. The member can switch a tool off or override its approval per tool
  (table `marketplace_tool_policy`, migration 0040). A destructive hint never
  gets a standing grant.
- **API keys.** Stored per member under `marketplace.<slug>.<secretKey>` in the
  encrypted store, never returned, never set as environment variables.
- **`suggest_marketplace_app`.** A read-only tool that renders an install card in
  chat.

Routes (all per member): `GET /v1/marketplace/plugins`, `GET .../plugins/{slug}`,
`POST .../install`, `.../enable`, `.../disable`, `.../config`,
`POST .../oauth/start` and `.../oauth/complete`, `GET .../accounts`,
`DELETE .../accounts/{account_id}`, `GET .../tools` and `.../tools/{tool}`.
See [Owner API](api.md).

## Shipped manifests

Real remote MCP hosts with OAuth (schema v2): Linear, Notion, Sentry, Vercel,
Stripe (read tools only), PostHog, Context7 and Atlassian. They are checked
against synthetic wiremock fixtures only. They are not verified until the owner
signs in and runs each one, because OAuth differs by host (DCR, CIMD or a fixed
client). GitHub is native, not a manifest: see [Connectors](connectors.md).

The remaining manifests (brave-search, elevenlabs, exa, firecrawl, github,
resend, tavily, workspace-files) still use placeholder hosts
(`*.example.invalid`). They are not real services and stay blocked: the desktop
offers no Install, Enable or secret entry for them. Canva is not shipped.

## Limits

- Only `http` transport installs. A `stdio` app is declared so the schema is
  complete, but install is refused: starting a local program per user on a
  shared server needs its own security design first.
- The desktop Marketplace screen (`RUNTIME_CALLS_PLUGINS = true`) offers Install,
  Sign in, an accounts list, state badges and per-tool switches for live `http`
  plugins only.
- Secrets are write-only; the admin cannot read them.
