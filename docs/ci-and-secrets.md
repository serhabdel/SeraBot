# Public CI and client secrets

The repo is public. CI must not use real owner tokens, API keys, or age identities.
Secret **values** stay in GitHub Actions (or the age store). This tree only names them.

## Rust (`.github/workflows/ci.yml`)

Linux and Windows: `cargo fmt`, `clippy -D warnings`, `cargo check --all-targets`, `cargo test`.
`SERABOT_TEST_MODE=1` for synthetic fixtures. No GitHub secrets required.

## Clients (`.github/workflows/clients.yml`)

Runs on push to `main`, pull requests, and manual dispatch. No Android SDK is
needed locally: this workflow is the Android compile verification.

- `serabot-desktop`: check, test, and release build on Ubuntu and Windows. On
  Windows only, the release exe is zipped as
  `serabot-desktop-<sha>-windows-x64.zip` and uploaded as the artifact
  `serabot-desktop-<sha>-windows-x64` with 14-day retention. The Windows binary
  is verified through this artifact, not by a local Windows build.
- `serabot_mobile`: Java 17 (temurin), `flutter pub get`, `flutter analyze`,
  `flutter test`, then `flutter build apk --debug` and an upload of
  `apps/serabot_mobile/build/app/outputs/flutter-apk/app-debug.apk` as
  `serabot-mobile-debug-<sha>` (14 days). This checks that Android compiles; it
  is not a distribution build.

The workflow accepts two optional repository secrets. When they are unset
(public CI, forks), the values are empty and the jobs still pass.

| Secret name | Synthetic example | What the workflow does |
| --- | --- | --- |
| `SERABOT_OWNER_API_URL` | `https://owner.example` | Passes the origin into the desktop build as `SERABOT_OWNER_API_URL`, and into Flutter as `SERABOT_DEFAULT_SERVER_URL`. |
| `SERABOT_OWNER_TOKEN` | a 32–512 character owner token you type later | **Not read.** Clients show this name so the owner knows which secret to type at runtime. |

Create the secrets in the GitHub repository settings. Do not put the values in a workflow file, a dart-define of the token, a commit, or a log.

Desktop prefill order: process environment `SERABOT_OWNER_API_URL`, then the value compiled in, then an empty field. Invalid origins (plain HTTP to a public host, credentials, paths) are ignored.

## Releases

`release.yml` builds and publishes the client downloads. It is normally
triggered by `ci.yml` after both test jobs pass on `main`: if the workspace
version in `Cargo.toml` has no matching `v<version>` tag, CI creates the tag on
that commit and dispatches `release.yml` with it. A version bump is therefore
the release switch. A manual tag push (`git tag vX.Y.Z && git push origin
vX.Y.Z`) or **Run workflow** with an existing tag (for example `v0.3.0`) still
works. Every job checks out that tag and publishes the GitHub Release with
`tag_name` set to it. Asset names use the same tag value.

| Asset | Built on | Notes |
| --- | --- | --- |
| `serabot-desktop-<tag>-windows-x64.exe` | `windows-latest` | Release binary |
| `serabot-desktop-<tag>-windows-x64.zip` | `windows-latest` | Zipped release binary |
| `serabot-desktop-latest-windows-x64.exe` | `windows-latest` | Stable alias of the same binary |
| `serabot-desktop-latest-windows-x64.zip` | `windows-latest` | Stable alias of the same zip |
| `serabot-mobile-<tag>.apk` | `ubuntu-latest` | `flutter build apk --release` |
| `serabot-mobile-<tag>.aab` | `ubuntu-latest` | `flutter build appbundle --release` (Play bundle) |
| `serabot-mobile-latest.apk` | `ubuntu-latest` | Stable alias |
| `serabot-mobile-latest.aab` | `ubuntu-latest` | Stable alias |
| `SHA256SUMS` | publish job | SHA-256 of every other file in `dist/`, generated after download |

The publish job downloads all artifacts, writes `SHA256SUMS` over the files
except itself, and uploads `dist/*` with `fail_on_unmatched_files: true`.
Android versions come from the workflow: `--build-name` strips the leading `v`
from the tag, and `--build-number` is `${{ github.run_number }}`, so
`versionCode` increases on every release run.

### Android release signing

These optional repository secrets are exposed to the mobile job environment.
Values must never be echoed, committed, or pasted into an issue.

| Secret name | Effect |
| --- | --- |
| `ANDROID_KEYSTORE_BASE64` | Base64 of the release keystore. When set, CI decodes it to `$RUNNER_TEMP/release.keystore` and exports `ANDROID_KEYSTORE_PATH` through `$GITHUB_ENV`; the secret value is never printed. |
| `ANDROID_KEYSTORE_PASSWORD` | Keystore password read by Gradle. |
| `ANDROID_KEY_ALIAS` | Key alias read by Gradle. |
| `ANDROID_KEY_PASSWORD` | Key password read by Gradle. |

`apps/serabot_mobile/android/app/build.gradle.kts` creates a `release` signing
config only when `ANDROID_KEYSTORE_PATH` is present. Otherwise the release
build falls back to the Android **debug** key, which installs but may not
upgrade in place because CI debug keys can change per run. Configure a stable
keystore before relying on APK updates.

### Build-time environment

| Variable | Value in CI | Effect |
| --- | --- | --- |
| `SERABOT_RELEASES_REPO` | `${{ github.repository }}` on desktop builds | Compile-time `owner/repo` used by the server for public download links. Client release builds receive the same value as build metadata. Local server builds for deployment must set `SERABOT_RELEASES_REPO=serhabdel/SeraBot`. |
| `SERABOT_BUILD_SHA` | The checked-out commit (`git rev-parse HEAD`) on desktop release builds | Reported by `/healthz` and `/v1/health` as `build`. Shows `dev` when unset. |

The server's public download routes redirect to
`https://github.com/<SERABOT_RELEASES_REPO>/releases/latest/download/<asset>`.
A malformed or absent value disables the links and makes the download routes
return `503`; it never accepts request input in a URL. See
[deployment](deployment.md) and [API](api.md).

## Tokens

| Client | Server URL | Token |
| --- | --- | --- |
| Desktop | Typed on first run; prefilled from `SERABOT_OWNER_API_URL` when CI compiled it | Minted per device with `serabot token mint --label desktop-win`. Stored in Windows Credential Manager via the `keyring` crate; only the server origin goes to `%APPDATA%\SeraBot\settings.json`. Debug builds may read `SERABOT_OWNER_TOKEN`; release builds contain no code that reads it. |
| Mobile | `--dart-define=SERABOT_DEFAULT_SERVER_URL` | Typed at runtime. Placeholder name `SERABOT_OWNER_TOKEN`. |
| Server | `[api].token_hash` remains as a static fallback | `serabot token mint/list/revoke` manages per-device hashes; `serabot hash-token` prints the static hash. |

The desktop token is not held in memory only: it persists in the OS credential
store so auto-connect works after restart. “Forget token” removes it. Treat a
minted token as a credential and hand it over through a secure channel.

Never bake owner token values into APKs or release binaries. `GITHUB_TOKEN`
needs no setup: the workflow declares `permissions: contents: write` and
creates the release from the tag.
