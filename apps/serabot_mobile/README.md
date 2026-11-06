# SeraBot mobile (Android)

Flutter owner console for the SeraBot HTTP API documented in [`docs/api.md`](../../docs/api.md). This is not a web admin UI: it connects to your existing server over HTTPS or a documented loopback/tunnel URL.

## Prerequisites

- Flutter stable (3.47+ tested)
- Android SDK / emulator or USB device
- A running SeraBot server with Microsoft sign-in configured

## Build and run

```bat
cd apps\serabot_mobile
flutter pub get
flutter test
flutter run
```

Release APK:

```bat
flutter build apk --release
```

The APK is under `build/app/outputs/flutter-apk/`.

## Signing in

Enter the **Server URL** (HTTPS origin, or exact loopback HTTP for SSH/`adb reverse` such as `http://127.0.0.1:8787`), then tap **Sign in with Microsoft**. The system browser opens, and the app receives the result on `serabot://auth` (PKCE-bound handoff, see `docs/specs/native-signin-contract.md`). Tick **I have an invite code** only for a first sign-in that needs one. There is no token to paste.

The session token is stored in the OS secret store (Keychain / Android Keystore) and is never logged. Any `401` clears it and returns to sign-in. **Sign out** revokes the session on the server and deletes the stored token.

### Emulator → host server

- **HTTPS tunnel** (recommended): paste the public HTTPS origin.
- **Loopback on device**: `adb reverse tcp:8787 tcp:8787` then use `http://127.0.0.1:8787`.

Plain HTTP is allowed only for loopback hosts (mirrors the desktop client).

## Features (MVP)

- Microsoft sign-in / sign-out (token in secure storage)
- Chat: send messages, poll server history (no optimistic/demo messages)
- Model catalog, selection with confirmation (respects `data_egress_approved`)
- Pending approvals list with once / always / deny
- Conservative polling (≥3s between requests) to stay under rate limits

## Security notes

- Redirects disabled; responses capped at 2 MiB
- Unapproved egress profiles visible but not selectable
- Lost submit responses reported as unknown outcome (no auto-resend)

## TODO

- SSE `/v1/events` stream instead of polling
- Task detail panel and artifact download / deck approve flows
- iOS build polish
- Biometric gate before restoring the stored session
