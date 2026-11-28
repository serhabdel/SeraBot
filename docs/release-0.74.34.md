# Release 0.74.34 — Claude Agent SDK: plan token sign-in and plan-first billing

## Fixed

- **Pasting a Claude token did nothing useful.** On the Claude Agent SDK
  (Gateway) card the *API key* tab saved the pasted text as the gateway's own
  bearer token, so a `claude setup-token` token (`sk-ant-oat01-…`) or an
  Anthropic key never reached the SDK and every turn got a 401 from the
  gateway. A pasted `sk-ant-oat…` / `sk-ant-api…` is now handed to the
  gateway (`POST /v1/auth/token`) and becomes its own account.
- **API key overriding the Pro/Max plan.** The gateway left a host
  `ANTHROPIC_API_KEY` in place next to a plan login; the SDK prefers it and
  billed pay-as-you-go. Plan logins (setup-token, `claude auth login`, a host
  `CLAUDE_CODE_OAUTH_TOKEN`) now remove it.
- Named gateway accounts no longer inherit the host's `CLAUDE_CODE_OAUTH_TOKEN`.

## How to use a Claude Pro/Max plan

1. On any computer signed in to your plan: `claude setup-token`.
2. Settings → AI providers → Claude Agent SDK (Gateway) → Add account →
   **Claude token or API key** → paste the `sk-ant-oat01-…` token.
3. Or use **Sign in with browser** (0.74.33). Both bill your plan's limits.

## Details

- Gateway: `POST /v1/auth/token` (bearer-auth), token stored per profile,
  0600, never echoed or logged; `auth/status` lists token accounts as signed
  in; the usage probe uses the plan token.
- Server: `add_provider_account` routes `sk-ant-…` keys for
  `claude-agent-sdk` to the gateway (`profile = tok_<sha256 prefix>`, so the
  same token is the same account); audited as `provider.account_added`
  `via: gateway_token`.
- Desktop: the tab reads **Claude token or API key** with `claude
  setup-token` instructions; preset note updated.

Not verified against a real Claude account here (synthetic tokens only).
