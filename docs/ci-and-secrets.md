# Public CI and client secrets

The repo is public. CI must not use real owner tokens, API keys, or age identities.
Secret **values** stay in GitHub Actions (or the age store). This tree only names them.

## Rust (`.github/workflows/ci.yml`)

Linux and Windows: `cargo fmt`, `clippy -D warnings`, `cargo check --all-targets`, `cargo test`.
`SERABOT_TEST_MODE=1` for synthetic fixtures. No GitHub secrets required.

## Clients (`.github/workflows/clients.yml`)

- `serabot-desktop`: check, test, and release build on Ubuntu and Windows
- `serabot_mobile`: `flutter analyze` and `flutter test` on Ubuntu

The workflow accepts two optional repository secrets. When they are unset (public CI, forks), the values are empty and the jobs still pass.

| Secret name | Synthetic example | What the workflow does |
| --- | --- | --- |
| `SERABOT_OWNER_API_URL` | `https://owner.example` | Passes the origin into the desktop build as `SERABOT_OWNER_API_URL`, and into Flutter as `SERABOT_DEFAULT_SERVER_URL`. |
| `SERABOT_OWNER_TOKEN` | a 32–512 character owner token you type later | **Not read.** Clients show this name so the owner knows which secret to type at runtime. |

Create the secrets in the GitHub repository settings. Do not put the values in a workflow file, a dart-define of the token, a commit, or a log.

Desktop prefill order: process environment `SERABOT_OWNER_API_URL`, then the value compiled in, then an empty field. Invalid origins (plain HTTP to a public host, credentials, paths) are ignored.

Mobile release build, when you choose to publish an APK locally or in a later job:

```bash
flutter build apk --release \
  --dart-define=SERABOT_DEFAULT_SERVER_URL="$SERABOT_OWNER_API_URL" \
  --dart-define=SERABOT_OWNER_TOKEN_SECRET_NAME=SERABOT_OWNER_TOKEN
```

`$SERABOT_OWNER_API_URL` is the origin only. Do not pass `SERABOT_OWNER_TOKEN` into `--dart-define`.

| Client | Server URL | Token |
| --- | --- | --- |
| Desktop | `SERABOT_OWNER_API_URL` (env, or compiled from the Actions secret) | Typed in the UI. Held in memory. Placeholder name `SERABOT_OWNER_TOKEN`. |
| Mobile | `--dart-define=SERABOT_DEFAULT_SERVER_URL` | Typed at runtime. Placeholder name `SERABOT_OWNER_TOKEN`. |
| Server | `[api].token_hash` locally | `serabot hash-token` |

Never bake owner token values into APKs or release binaries.

## Releases (`.github/workflows/release.yml`)

Any tag push matching `v*` builds the clients and posts a GitHub Release with
generated notes:

```bash
git tag v0.1.0 && git push origin v0.1.0
```

| Asset | Built on | Command |
| --- | --- | --- |
| `serabot-desktop-<tag>-windows-x64.zip` | `windows-latest` | `cargo build --release --locked -p serabot-desktop`, zipped |
| `serabot-desktop.exe` | `windows-latest` | Same build, bare binary |
| `serabot-mobile-<tag>.apk` | `ubuntu-latest` | `flutter build apk --release` in `apps/serabot_mobile` |
| `serabot-mobile-<tag>.aab` | `ubuntu-latest` | `flutter build appbundle --release` (Play bundle) |

Download from the repository **Releases** page (latest tag on top). Windows:
unzip and run `serabot-desktop.exe`. Android: allow "install unknown apps" and
sideload the APK. The APK and AAB are signed with the Android **debug** key
(the TODO in `apps/serabot_mobile/android/app/build.gradle.kts`), so they
sideload fine but are not Play Store uploads. A real signing config would add
keystore secrets; that is a follow-up and is not wired here.

Release secrets — the same optional names as the client CI, nothing else:

| Secret name | Read by the release workflow | Effect |
| --- | --- | --- |
| `SERABOT_OWNER_API_URL` | Yes | Compiled into the desktop binary and passed to Flutter as `--dart-define=SERABOT_DEFAULT_SERVER_URL`. Origin only. Empty or invalid values are ignored at runtime, so unset secrets still produce working clients with a generic server field. |
| `SERABOT_OWNER_TOKEN` | **No** | Never read, compiled, or embedded. The clients name it so the owner knows which secret to type at runtime. |

`GITHUB_TOKEN` needs no setup: the workflow declares `permissions: contents:
write` and creates the release from the tag. No other secrets are required for
public artifacts.
