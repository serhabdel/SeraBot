# Public CI and client secrets

The repo is public. CI must not use real owner tokens, API keys, or age identities.

## Rust (`.github/workflows/ci.yml`)

Linux and Windows: `cargo fmt`, `clippy -D warnings`, `cargo check --all-targets`, `cargo test`.
`SERABOT_TEST_MODE=1` for synthetic fixtures. No GitHub secrets required.

## Clients (`.github/workflows/clients.yml`)

- `serabot-desktop`: check + test on Ubuntu and Windows
- `serabot_mobile`: `flutter analyze` + `flutter test` on Ubuntu

| Client | Server URL | Token |
| --- | --- | --- |
| Desktop | `SERABOT_OWNER_API_URL` env hint | UI only, memory |
| Mobile | `--dart-define=SERABOT_DEFAULT_SERVER_URL` | Runtime entry only |
| Server | `[api].token_hash` locally | `serabot hash-token` |

Never bake owner tokens into APKs or release binaries.
