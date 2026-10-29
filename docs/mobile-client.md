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
