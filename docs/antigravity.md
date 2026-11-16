# Google Antigravity / Cloud Code

`google-antigravity` talks to Google Cloud Code Private Assist over HTTPS. SeraBot does not shell out to a proprietary binary. Chat uses the documented JSON envelope. The model list is a separate POST. Neither path was exercised against a live Google account in this tree; tests use synthetic fixtures.

Since 0.49 every member signs in to Antigravity from the desktop and keeps their own accounts (several per member). Each account's tokens are sealed under that member's own key; the age store keeps only the OAuth client secret. Errors, events and audit rows do not include access tokens, refresh tokens, authorization codes, or PKCE verifiers. Antigravity is a vendor-internal endpoint: it can change or ban without notice, and an account that Google refuses shows `needs_reauth`.

## Member sign-in (0.49)

The desktop starts `POST /v1/providers/{id}/signin/start` (`flow: "pkce"`), opens the returned Google URL, catches the redirect on `http://127.0.0.1:<port>/oauth-callback` (51121 first, any free loopback port if that one is busy), and sends the code to `POST .../signin/complete`. The server follows the reference client exactly:

- scopes are `cloud-platform`, `userinfo.email`, `userinfo.profile`, `cclog` and `experimentsandconfigs`, and never `openid` (Google hangs a desktop PKCE flow that asks for it); `access_type=offline` and `prompt=consent` make Google return a refresh token, and a login without one is refused;
- the OAuth client secret comes from the age store (`antigravity_oauth_client_secret`, or the provider's `oauth_client_secret_secret`); without it sign-in answers "not set up on this server";
- the email comes from `GET https://www.googleapis.com/oauth2/v1/userinfo?alt=json` and names the account (a login with no email and no Google id is refused);
- project discovery runs `loadCodeAssist` and, when the account has no project, `onboardUser` up to three times, three seconds apart, asking the daily host and then prod. The project and tier are stored in the account's identity. A member's account never uses the provider's configured `project_id`; an account whose discovery found nothing omits `project` and looks again on first use (at most once an hour).

The owner's `POST /v1/admin/providers/{id}/oauth/start|complete` routes now do the same for the owner's own account in the per-user store; there is no restart and no age-store write. A new sign-in the operator stores with `serabot providers login` is added to the admin's accounts at the next start.

## Wire details

- `sessionId` is FNV-1a 64 over `"{project}:{conversation}"` (`":gen{N}"` appended after a rotation), folded `% 9e18` and prefixed `-`. On Google's `exceeds the maximum number of tokens` 400 the account's `session_generation` is bumped once and the request retried; the generation lives in the account row.
- `generationConfig`: `maxOutputTokens` takes the model's cap (64000 Claude, 65536 others) when the configured value is 4096 or less. Gemini 3 and 4 level models map the bot's thinking level to `thinkingLevel` (`max` sends `high`, `off` sends `thinkingBudget: 0`); an id ending in `-low`, `-medium` or `-high` sends no thinking config. Claude uses budgets of 4000, 10000, 24000 and 32000 tokens kept inside `[1024, maxOutputTokens - 1024]`. A temperature is dropped for Claude while it thinks.
- Requests to Claude models never end on a model turn. `functionResponse.is_error` reflects the tool's real outcome.
- `thought: true` parts are reasoning (display data, never an answer). A function call's `thoughtSignature` is stored with that call in the turn and sent back with it; the sentinel stands in only when there is none.
- `MALFORMED_FUNCTION_CALL` with no content is sent again, up to three attempts in all.
- Our `web_search` and `search_web` tools travel as `agy_web_search` and `agy_search_web` and are mapped back; `google_search` is never offered. Tool schemas pass an allow-list (union `type` arrays become `nullable`, `const` becomes `enum`, `required` keeps only existing properties).
- The catalog skips tab, image, audio and deprecated ids and the legacy `claude-*-4-6` models, and carries `maxTokens` and `maxOutputTokens` into each model's limits.
- Quota: `POST /v1internal:retrieveUserQuotaSummary`, with `fetchAvailableModels` `quotaInfo` as the fallback, is cached in the account's `quota_json` (`GET /v1/providers/{id}/accounts/{aid}/quota?refresh=1`, at most once every four minutes). Any failure is "no reading".
- The user agent's Hub version is pinned in the build and can be set with `[llm.providers.<id>] client_version = "2.25.0"`. SeraBot does not fetch an updater URL.

None of this was run against a live Google account; tests use synthetic fixtures.

## Operator login from the command line

Full browser UI is out of scope. Login is a loopback PKCE flow, the same shape as the reference app's Antigravity sign-in:

1. Store the published Antigravity desktop OAuth **client secret** (the secret that pairs with the built-in client id). Do not commit it.

   ```text
   serabot secrets set antigravity_oauth_client_secret
   ```

   Paste the secret on stdin. Leave `oauth_client_id_secret` empty so refresh uses the built-in client id. If you set that secret, it must be exactly that client id or login refuses.

2. Point a provider at Cloud Code. `base_url` may be empty (the daily host below) or one of the documented origins. `project_id` may stay empty until login prints one.

   ```toml
   [llm.providers.antigravity]
   kind = "google-antigravity"
   auth = "oauth"
   api_key_secret = "antigravity_access_token"
   refresh_token_secret = "antigravity_refresh_token"
   oauth_client_secret_secret = "antigravity_oauth_client_secret"
   project_id = ""
   models = ["example-model"]
   catalog_fetch = true
   data_egress_approved = false
   ```

3. Run:

   ```text
   serabot providers login google-antigravity
   ```

   SeraBot listens on `127.0.0.1:51121` (or an ephemeral loopback port if that port is taken), prints an `https://accounts.google.com/o/oauth2/v2/auth` URL, and waits up to five minutes. Open that URL, approve access, and let the browser hit the loopback redirect. SeraBot exchanges the code with `https://oauth2.googleapis.com/token` (`authorization_code`, S256 PKCE, `access_type=offline`, `prompt=consent`) and stores the access token and refresh token under the configured secret names. It prints those names, not the token values.

4. If Cloud Code returns a companion project id, the command prints `project_id=...`. Copy that value into the provider. An empty `project_id` omits the `project` field. Sending `"project": ""` makes Cloud Code return 404, so the field is omitted rather than blank.

A refresh token you already hold can still be stored with `serabot secrets set` and refreshed on a 401. PKCE is the way to obtain one. Login does not read Antigravity CLI credential files.

When Google returns a new refresh token, SeraBot writes that refresh token and the new access token back to the configured age-store keys. A failed write fails the call. The values are not logged.

## Hosts

Chat and catalog contact only these origins, starting with the configured one (default first):

| Order | Origin |
| --- | --- |
| 1 | `https://daily-cloudcode-pa.googleapis.com` |
| 2 | `https://cloudcode-pa.googleapis.com` |
| 3 | `https://daily-cloudcode-pa.sandbox.googleapis.com` |
| 4 | `https://autopush-cloudcode-pa.sandbox.googleapis.com` |

Other provider kinds still reject these hosts. Redirects are disabled. `403`, `404`, `429`, and `5xx` on a catalog call try the next origin. Chat tries `POST /v1internal:streamGenerateContent?alt=sse` and, on 404, `POST /v1internal:generateContent`, then the next origin for `403`, `429`, and `5xx`. A 401 refreshes the Google token once and retries. Response bodies are dropped on errors.

## Catalog

With `catalog_fetch = true` and `data_egress_approved = true`, SeraBot POSTs `{ "project": "<project_id>" }` (or `{}` when the project id is empty) to `/v1internal:fetchAvailableModels`. The cache, TTL, allowlist intersection, and fail-closed behavior match the other official catalogs.

The body must be a `models` object. Ids come from the object's keys. Image, audio, tab, and deprecated ids are skipped. An OpenAI-style `{ "object": "list", "data": [...] }` body is rejected and the cache closes. Allowlist ids are not invented when the fetch fails.

## Chat

The turn loop still waits for one assistant message. SeraBot buffers the SSE stream (or the JSON fallback) and does not forward chunks to the owner.

The request is a Cloud Code envelope: `model`, optional `project`, `request.contents`, `systemInstruction`, sanitized tool declarations, and `generationConfig`. Tool schemas are reduced to the uppercase protobuf subset Cloud Code accepts (`OBJECT`, `STRING`, and the other scalar types). Empty objects gain a placeholder property. Unsupported JSON Schema keywords are stripped.

Function-call parts send the `thoughtSignature` Google gave that call, or `skip_thought_signature_validator` when there is none (see Wire details). `claude` models whose ids contain `thinking` also send `anthropic-beta: interleaved-thinking-2025-05-14`; the reference client's current code does not, so this stays only until a fixture shows it is unnecessary. Model ids in config must be ids the live catalog actually returns; SeraBot does not rewrite them.

## Deferred

- Image generation, image edit and `googleSearch` grounding.
- Reading tokens out of a local Antigravity CLI or IDE profile.
