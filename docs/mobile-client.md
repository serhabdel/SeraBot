# Mobile client (Android)

Flutter owner console: `apps/serabot_mobile`.

Android builds are **CI-only**. Do not install an Android SDK or run `flutter
build` locally; the GitHub Actions runner is the only Android compile
verification path.

| Trigger | Workflow | Output |
| --- | --- | --- |
| Push to `main`, pull request, manual dispatch | `clients.yml` | `flutter analyze`, `flutter test`, then a debug APK uploaded as the artifact `serabot-mobile-debug-<sha>` (14 days) |
| Tag push (`v*`) or `release.yml` dispatch with a `tag` input | `release.yml` | Versioned and `latest` APK/AAB release assets |

Local Dart checks are optional and need no Android SDK:

```bash
cd apps/serabot_mobile
flutter pub get && flutter analyze && flutter test
```

The app connects to the owner API at the build-time
`SERABOT_DEFAULT_SERVER_URL`. The owner release build pre-fills
`https://bot.seradyx.tech` from the `SERABOT_OWNER_API_URL` repository secret;
forks build a generic client with an empty default. The token is entered at
runtime only and is never embedded. The connect screen names the GitHub
Actions secret `SERABOT_OWNER_TOKEN`; its value is not compiled in.

Release builds take their version from the release tag:
`--build-name="${TAG#v}"` and `--build-number="${{ github.run_number }}"`, so
`versionCode` increases on every CI run. The workspace `pubspec.yaml` version
is not changed by this workflow.

Signing: a stable release keystore is optional. When
`ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`,
and `ANDROID_KEY_PASSWORD` are configured, CI decodes the keystore to a
temporary path and Gradle signs the release build with it. Secret values are
never printed. Without those secrets, Gradle falls back to the Android debug
key; such APKs install but cannot replace a differently signed install, and
the debug key may change between CI runs. Use a stable keystore before relying
on in-place upgrades.

Prebuilt downloads are attached to GitHub Releases on tag pushes or manual
dispatch; see [CI and secrets](ci-and-secrets.md#releases). The server also
redirects `https://bot.seradyx.tech/app/latest.apk` and
`https://bot.seradyx.tech/app/SHA256SUMS` to the latest release, and the
landing page at `https://bot.seradyx.tech/` links both clients. Allow "install
unknown apps" and sideload the APK; this is not a Play Store upload.

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
