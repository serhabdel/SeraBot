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

## Not in this client yet

Creating a disabled specialist (`POST /v1/bots`) and a conversation node
(`POST /v1/conversations`) is on the desktop Fleet panel. See
[Desktop client](desktop-client.md). This Flutter app does not call those
endpoints yet.
