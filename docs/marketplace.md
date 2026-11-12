# Plugin marketplace

SeraBot plugins are described by a `plugin.json` file in `plugins/<slug>/`.
The catalog is read at boot and validated; a file that fails is reported and
skipped, and the others still load.

## Design credit and licensing

The layout (a catalog of plugin folders, each with one manifest, and apps with
connection and authentication options) was modelled on a public plugin catalog
whose license is noncommercial. No text, descriptions, or code from that catalog
are copied. Every descriptive field here was written for SeraBot, and the
schema is SeraBot's own, version 1.

## Schema version 1

| Field | Meaning |
|-------|---------|
| `schemaVersion` | Must be `1`. |
| `slug` | Lowercase letters, digits and dashes; must match the folder name. |
| `name`, `tagline`, `description`, `category` | Display text. |
| `iconUrl`, `websiteUrl`, `privacyPolicyUrl`, `termsUrl` | HTTPS URLs without credentials. |
| `version`, `minServerVersion` | Dotted numbers such as `1.0.0`. |
| `prompts` | Starter prompts (`id`, `text`). |
| `apps[]` | Each app has `id`, `name`, a `server` and `auth` options. |
| `apps[].server` | `transport: "http"` with `url`, or `transport: "stdio"` with `command` and `args`. |
| `apps[].auth[]` | `kind` is `oauth-link` or `api-key`. Key fields name a `secretKey`. |
| `permissions.egress` | Bare host names the plugin may reach. |

## Current limits

- Only `http` transport installs. A `stdio` app is declared so the schema is
  complete, but install is refused: starting a local program per user on a
  shared server needs its own security design first.
- Secrets are stored per user under `marketplace.<slug>.<secretKey>` in the
  encrypted per-user store. They are never returned, and never set as
  environment variables.
- The tool runtime that calls a plugin's server is not part of this release.
- The host names in the shipped catalog are placeholders until each provider's
  real host, terms and privacy URLs are checked.
- The desktop Marketplace screen lists the catalog and says the plugins are
  coming soon. It does not send Install or Enable, and it does not ask for a
  secret, until a plugin can actually be called. Placeholder hosts stay
  blocked after the runtime exists.
