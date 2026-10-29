# SeraBot mobile (Android)

Flutter owner console for the SeraBot HTTP API documented in [`docs/api.md`](../../docs/api.md). This is not a web admin UI: it connects to your existing server over HTTPS or a documented loopback/tunnel URL.

## Prerequisites

- Flutter stable (3.47+ tested)
- Android SDK / emulator or USB device
- A running SeraBot server with `[api].token_hash` configured

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

## Connecting

| Field | Notes |
| --- | --- |
| **Server URL** | HTTPS origin (`https://your-tunnel.example`), or exact loopback HTTP for SSH/`adb reverse` (`http://127.0.0.1:8787`, `http://localhost:8787`, `http://[::1]:8787`). No path, query, credentials, or redirects. |
| **API token** | Owner token (32–512 visible ASCII chars). Held **in memory only**; cleared on disconnect. Never logged. |
| **Conversation id** | Same rules as the API (`android-owner` default). |

### Emulator → host server

- **HTTPS tunnel** (recommended): paste the public HTTPS origin.
- **Loopback on device**: `adb reverse tcp:8787 tcp:8787` then use `http://127.0.0.1:8787`.

Plain HTTP is allowed only for loopback hosts (mirrors the desktop client).

## Features (MVP)

- Connect / disconnect (session wipe on disconnect)
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
- Secure enclave / biometric gate before showing token field
