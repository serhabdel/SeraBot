# Release 0.51.0

Marketplace with real sign-in, a native GitHub connector, and Codex and GitHub
Copilot plan sign-in. Details: [Marketplace](marketplace.md),
[Connectors](connectors.md), [Providers](providers.md).

## Marketplace

- **Manifest schema v2** (v1 still loads): `oauth` auth, header deny list, tool
  `allow`/`deny`/`readOnly`, `accounts.multiple`.
- **MCP OAuth** (`mcp_oauth.rs`): RFC 9728/8414 discovery, CIMD then DCR then a
  configured client id, PKCE S256 with an RFC 8707 resource, public-IP checks,
  64 KiB and 20 s bounds, a defined outcome per failure, RFC 7009 revoke on
  uninstall.
- **Accounts and tool policy**: migrations 0039 (`marketplace_accounts`,
  `github_connector`) and 0040 (`marketplace_tool_policy`). Routes for OAuth
  start/complete, accounts and per-tool policy.
- **Runtime**: auth chosen from the manifest, bearer injected at call time with
  one refresh-and-retry on 401, every `http` app used, read-only tools skip
  approval (still audited and scanned), failures reported instead of swallowed.
- **Real hosts**: Linear, Notion, Sentry, Vercel, Stripe (read tools only),
  PostHog, Context7, Atlassian.
- **`suggest_marketplace_app`**: read-only install card in chat.
- **Desktop**: sign-in button, accounts list, state badges, per-tool switches and
  a GitHub card.

## GitHub (native)

Device flow for a GitHub App (`[connectors.github] app_client_id`, public, no
secret), tokens sealed per member, refreshed ten minutes early with one refresh
at a time, tools from GitHub's hosted MCP as `github.<tool>` (writes need
approval bound to the repository and arguments). The pasted-token connector
stays as the fallback.

## Codex and GitHub Copilot sign-in

Both are on by default with a risk banner (owner decision; `plan_oauth = false`
turns either off) and are open to every member, each account its member's own
and sealed. Mistral sign-in is not wanted and was not built.

- **ChatGPT (Codex)**: browser PKCE on `localhost:1455` or a device code; the
  Responses transport; several accounts; rotating refresh through the
  single-flight refresher.
- **GitHub Copilot**: GitHub device code, then the Copilot token exchange; the
  plan's own API host; chat completions; several accounts; a GitHub account
  without Copilot ends `copilot_not_subscribed`.

## Not done

- Canva manifest (named in the spec) is not shipped.
- Copilot: the `auto` model, Responses-only models, per-model reasoning effort,
  the quota meter and a refresh that picks up a changed plan host.
- Codex: encrypted reasoning items between tool rounds, the plan usage meter.
- Placeholder-host manifests remain placeholders; `stdio` plugins stay off.

## Not verified

Everything runs against synthetic fixtures (wiremock). No live check was made of
any of the following, and none of them may be called working until the owner
runs it:

- MCP OAuth against each real host (Linear, Notion, Sentry, Vercel, Stripe,
  PostHog, Context7, Atlassian), including DCR or CIMD acceptance and scopes.
- The GitHub device flow, token refresh and the hosted GitHub MCP (its terms
  were not reviewed).
- ChatGPT/Codex sign-in and chat, and GitHub Copilot sign-in, exchange and
  chat. Client ids, editor headers and versions are copied from the owner's
  HielCowork as they stand and may be restricted by OpenAI or GitHub.
- The desktop screens were not visually checked on a device.
