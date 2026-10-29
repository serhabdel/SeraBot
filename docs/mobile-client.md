# Mobile client (Android)

Flutter owner console: `apps/serabot_mobile`.

```bash
cd apps/serabot_mobile
flutter pub get && flutter analyze && flutter test
```

Connect with HTTPS to the VPS or loopback over SSH/`adb reverse`. The token is entered at runtime only. The connect screen names the GitHub Actions secret `SERABOT_OWNER_TOKEN` and does not embed its value.

Optional build hint (URL only, never the token value):

```bash
flutter build apk --release \
  --dart-define=SERABOT_DEFAULT_SERVER_URL=https://owner.example \
  --dart-define=SERABOT_OWNER_TOKEN_SECRET_NAME=SERABOT_OWNER_TOKEN
```

`https://owner.example` is a synthetic origin. CI reads the optional repository secret `SERABOT_OWNER_API_URL` and passes it as `SERABOT_DEFAULT_SERVER_URL`. An invalid origin is ignored and the field stays `https://`. See `docs/ci-and-secrets.md`.

## Fleet

The **Fleet** tab calls `POST /v1/bots`, `POST /v1/skills`, and
`POST /v1/conversations`, matching the desktop specialist, skill, and
conversation forms.
Each submit waits at least three seconds after the previous Fleet request and
is not retried when the outcome is unknown.

- **New specialist** — `POST /v1/bots` with a lowercase bot id, a description
  (stored in the prompt stub), tools chosen from the specialist allowlist, and
  optional comma-separated skill ids. `delegate` is not offered. The server
  writes a disabled specialist. Restart before it can run, and add it to
  `main.delegates` yourself if `main` should hand it work.
- **New skill** — `POST /v1/skills` with a lowercase skill id, a one-line
  description, and an optional markdown body. There is no path field. The
  server writes the pack under the configured skills directory and reports
  `live: false`. Restart, then add the id to a bot's skills list before that
  bot can load it. **Refresh skills** calls `GET /v1/skills` for on-disk
  summaries. Bodies are not returned.
- **New conversation** — `POST /v1/conversations` with kind (`section`, `group`,
  `direct`, or `room`), a slug, and an optional parent. Sections have no parent.
  Groups require a section id (`sec:…`). Direct and room nodes may sit under a
  section or a group. The server assigns the id. **Use in chat** copies that id
  into the chat conversation and opens Chat. Sections and groups do not accept
  turns.

See [Desktop client](desktop-client.md).
