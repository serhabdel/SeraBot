# Release 0.48.0

Hardening release following the 0.39.0-0.47.0 review. This file lists the
logging, approval-binding, and test-coverage changes; other areas of 0.48.0
add their own sections below.

## Highlights

- **Logging** (`crates/serabot/src/logging.rs`, `crates/serabot/src/main.rs`, `crates/serabot/src/config.rs`, `serabot.toml`):
  - `rustls`, `hyper`, `hyper_util`, `h2`, `reqwest` and `tokio_util` are always capped at `warn` unless the filter names them, so a bare `RUST_LOG=trace` no longer fills the files, stdout, or the console Logs tab with TLS/HTTP handshake lines.
  - One `DEFAULT_FILTER` (`serabot=info,tower_http=warn`, the same as `deploy/serabot.service`) is shared by `logging.rs` and the stdout fallback in `main.rs`.
  - The ring buffer layer now has its own per-layer filter like the file and stdout layers.
  - New `[logging]` section: `dir` (default `data/logs` beside the config, no longer the OS temp dir) and optional `level`. `RUST_LOG` still wins. The directory is created `0700` and files `0600` on Unix.
- **Approvals** (`crates/serabot/src/gateway.rs`): `decide` refuses an approval whose `expires_at` has passed even if the expiry sweep has not run yet, and expires it.
- **Negative tests**: job tools and workflows/routines across users, artifact preview for a foreign artifact and a traversal-shaped hash, approval expiry / changed arguments / foreign user, vault autofill resolution across users and look-alike hosts, real-process check that the password manager CLI child environment is cleared.
- **Jobfinder**: `require_approval = []` matches the other bots; a test fails if an upload/send/execute tool is ever granted without being listed for approval.

## Upgrade notes

- Existing deployments that relied on `std::env::temp_dir()/serabot-logs` find new logs under `data/logs`. Set `[logging] dir` to keep another location.

## Upgrade note: restored built-in bots

Built-in bots a user deleted before 0.48.0 have no `removed` flag, so the first
upgrade restores them (enabled). Owners can disable or remove them again from
Integrations → Bots; removal is remembered from 0.48.0 on.
