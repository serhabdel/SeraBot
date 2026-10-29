# Mobile client (Android)

Flutter owner console: `apps/serabot_mobile`.

```bash
cd apps/serabot_mobile
flutter pub get && flutter analyze && flutter test
```

Connect with HTTPS to the VPS or loopback over SSH/`adb reverse`. Token is entered at runtime only.

Optional build hint (URL only, never token):

```bash
flutter build apk --release --dart-define=SERABOT_DEFAULT_SERVER_URL=https://your-vps.example
```

See `docs/ci-and-secrets.md`.

## Fleet

The **Fleet** tab calls the same owner endpoints as the desktop Fleet panel.
Each submit waits at least three seconds after the previous Fleet request and
is not retried when the outcome is unknown.

- **New specialist** — `POST /v1/bots` with a lowercase bot id, a description
  (stored in the prompt stub), tools chosen from the specialist allowlist, and
  optional comma-separated skill ids. `delegate` is not offered. The server
  writes a disabled specialist. Restart before it can run, and add it to
  `main.delegates` yourself if `main` should hand it work.
- **New conversation** — `POST /v1/conversations` with kind (`section`, `group`,
  `direct`, or `room`), a slug, and an optional parent. Sections have no parent.
  Groups require a section id (`sec:…`). Direct and room nodes may sit under a
  section or a group. The server assigns the id. **Use in chat** copies that id
  into the chat conversation and opens Chat. Sections and groups do not accept
  turns.

See [Desktop client](desktop-client.md).
