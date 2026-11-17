# Release 0.53.1

Antigravity sign-in and chat brought in line with the owner's HielCowork, which
signs in and chats with the same Google client.

## Sign-in

- The redirect is now `http://localhost:<port>/oauth-callback` (51121 first,
  then any free loopback port), not `http://127.0.0.1:...`. Google matches the
  redirect host against the installed-app registration, and HielCowork's
  working sign-in uses `localhost`. The server still accepts the `127.0.0.1`
  spelling from older consoles.
- The desktop and `serabot providers login` listen on both `127.0.0.1` and
  `[::1]` at that port, because a browser may resolve `localhost` to either.
- The OAuth client id can be overridden with `AGY_CLIENT_ID` (after the
  provider's own `oauth_client_id_secret`), the same way `AGY_CLIENT_SECRET`
  already overrides the secret. Sign-in and refresh use the same id.

## Client version

The Hub version in the user agent and `ideVersion` follows HielCowork's order:
the provider's `client_version`, then `AGY_VERSION`, then the version the public
Antigravity auto-updater reports (asked once per process, three-second timeout,
no redirects, used only when it is a plain dotted version), then the pinned
`2.25.0`. Google retires old client versions, so an aging pin no longer gets a
server refused.

## Chat and catalog

- Plain `claude-opus-5-5` refused with a 400 or 404 is sent once more as
  `claude-opus-5-5-thinking`, the id Cloud Code serves it under. No other id is
  rewritten; a second refusal is reported as it is.
- Models listed on an Antigravity provider join a verified live list instead of
  being dropped from it, as HielCowork appends its presets: Google's list leaves
  out ids that still answer. Malformed and skipped legacy ids stay refused.

## A chat never just spins

- Model calls now time out when the provider goes quiet for the configured
  time (default 120 s), not when an answer takes longer than that. A long
  thinking answer from Claude Opus through Antigravity used to be cut at
  120 s, then tried on the next Google host, and the chat spun for minutes. A
  whole call is still bounded at 15 minutes.
- When a model call fails, the chat says so at once with the status and the
  failure type (for example "status 429 (rate limited)"), and whether the
  turn will try again. The provider's reply body is still never shown: it can
  echo credentials.
- A turn waiting for a model on hold says until when, and the last error.
- No `anthropic-beta` header is sent to Cloud Code, as in HielCowork.

## Claude plan sign-in

Anthropic accepts a Claude plan sign-in only inside its own apps, so the
server's direct calls with it end up cooling down. SeraBot will not pretend to
be Claude Code. The supported way, running the real Claude Code on the owner's
computer through the Agent Client Protocol, is proposed in
`docs/specs/claude-code-acp.md` for 0.54. Until then use a Claude API key, or
the Claude models offered through Antigravity.

## Not copied

HielCowork's Google client secret is not copied into this public repository.
The build still takes it from `SERABOT_AGY_CLIENT_SECRET` (CI) or
`config/agy.local.toml`, and a deployed server can take it from the AI
providers page or `AGY_CLIENT_SECRET`.

## Not verified

No Google account was used. The redirect, listeners, version order, wire-id
retry and catalog union are covered by synthetic tests only; the owner should
sign in once from the desktop before calling Antigravity working.
