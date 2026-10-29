# Google Antigravity / Cloud Code

`google-antigravity` talks to Google Cloud Code Private Assist over HTTPS. SeraBot does not shell out to a proprietary binary. Chat uses the documented JSON envelope. The model list is a separate POST. Neither path was exercised against a live Google account in this tree; tests use synthetic fixtures.

Tokens stay in the age secret store. Errors and logs do not include access tokens, refresh tokens, authorization codes, or PKCE verifiers.

## One-time owner login

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

Function-call parts send `thoughtSignature` set to `skip_thought_signature_validator`. That is the reference fallback for calls that were not signed by this provider. SeraBot does not store thought signatures, so a later turn cannot replay a cryptographic signature from an earlier one.

`claude` models whose ids contain `thinking` send `anthropic-beta: interleaved-thinking-2025-05-14` and a thinking budget. Gemini 3 and Gemini 4 ids send a medium thinking level. Model ids in config must be ids the live catalog actually returns; SeraBot does not rewrite them.

## Deferred

- Image generation, image edit, `googleSearch` grounding, and quota telemetry.
- Persisting a rotated refresh token back into the age store. A new refresh token is kept for the life of the process. If refresh fails after a restart, run login again.
- Reading tokens out of a local Antigravity CLI or IDE profile.
