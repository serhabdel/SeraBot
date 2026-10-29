# Deployment (OVH, Ubuntu 24.04)

SeraBot runs as one systemd service with a single dynamically linked binary. No
Docker, Postgres, Node, or web UI is installed by these steps. Existing
services, containers, tunnels, and data must not be removed, restarted, or
reconfigured.

> **Policy:** OVH is read-only unless the owner explicitly authorizes an
> additive installation. Prepare the release locally and present the exact
> manifest (files, paths, commands, rollback) before any remote change.

## Host facts

- Ubuntu 24.04 (glibc 2.39), 2 vCPU, ~3.8 GB RAM.
- Service: `serabot.service` (enabled) at `/opt/serabot/serabot`.
- Configuration: `/etc/serabot/serabot.toml`; secrets in `/etc/serabot/private`.
- Data, models, artifacts: `/var/lib/serabot`.
- Owner API: loopback `127.0.0.1:8787`; reach it through an SSH tunnel or the
  approved TLS proxy. Never expose it publicly.
- Disk headroom is limited; keep old binary backups only as long as needed.

## Build the release on Ubuntu 24.04

Building on a newer distribution produces a binary that requires a newer glibc
and will fail on the server.

```bash
source ~/.cargo/env
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
cargo build --release --locked -p serabot
```

The server binary is `target/release/serabot`. The desktop crate is a workspace
member but is not deployed; `-p serabot` keeps the release server-only.

Verify the artifact before upload:

```bash
file target/release/serabot
sha256sum target/release/serabot
ldd target/release/serabot | head
```

## Upgrade procedure (additive, single service)

1. Read-only preflight on the host:

   ```bash
   ssh ovh-vps 'systemctl is-active serabot; systemctl is-enabled serabot; df -h /; ls -l /opt/serabot/serabot'
   ```

2. Upload the binary to a staging path, for example
   `/var/lib/serabot/releases/serabot-<short-sha>`.
3. Back up the current binary to `/var/lib/serabot/releases/serabot-<timestamp>`.
4. Replace `/opt/serabot/serabot` only after verifying the staged SHA-256.
5. Restart only `serabot.service`.
6. Verify:

   ```bash
   systemctl status serabot --no-pager
   journalctl -u serabot -n 50 --no-pager
   ssh -L 8787:127.0.0.1:8787 ovh-vps
   curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8787/v1/health
   ```

7. Roll back by restoring the previous binary and restarting only this service.

Do not touch Docker, OpenBot, the OneDrive MCP service, or the Cloudflare tunnel.

## Configuration compatibility

The new routing fields are optional and default to the previous behavior:

```toml
[llm]
models = []                 # extra allowed models for the "default" provider
# [llm.providers.<id>]      # optional additional profiles
```

`data_egress_approved` remains `false` unless explicitly approved by the owner. `catalog_fetch` fails closed: an unapproved provider is not contacted, and a failed catalog refresh advertises no models.
If no extra profiles are configured, the catalog contains only `default` and
per-bot model overrides.

Upgrade note: queued turns now carry a pinned route. Legacy queued turns without
a route fail closed and must be resubmitted. With live conversational use
disabled and an empty queue there is nothing to drain; confirm with
`/v1/status` before upgrading a busy instance.

## Secrets

Never print, commit, or paste secrets into prompts or tickets. Use the encrypted
store as the service user:

```bash
runuser -u serabot -- /opt/serabot/serabot --config /etc/serabot/serabot.toml secrets init
printf '%s' "$KEY" | runuser -u serabot -- /opt/serabot/serabot --config /etc/serabot/serabot.toml secrets set llm_api_key
```

`secrets set` reads the value from stdin; redirecting a file is non-interactive,
while a bare invocation waits for EOF. Adding an extra provider requires storing
its referenced secret before the service can start.

## OneDrive (native Graph)

SeraBot uses the same Azure **public client** pattern as the existing
`onedrive-mcp.service` on the host (`AZURE_CLIENT_ID` in
`/home/ubuntu/onedrive-mcp/.env`, MSAL device-code auth, refresh tokens in
`TOKEN_CACHE_PATH` / `.token_cache.bin`). Do not stop or reconfigure the MCP
service; native Graph is an additive path through the gateway.

Keep `[onedrive].enabled = false` until both secrets exist in the SeraBot age
store. Align `[onedrive].tenant` with the MCP authority (`consumers` for personal
OneDrive). Secret names default to `onedrive_client_id` and
`onedrive_refresh_token`.

On the VPS (as a user who can read the MCP env, never paste values into chat):

1. Copy the MCP application id into SeraBot:

   ```bash
   CLIENT_ID="$(grep '^AZURE_CLIENT_ID=' /home/ubuntu/onedrive-mcp/.env | cut -d= -f2-)"
   printf '%s' "$CLIENT_ID" | runuser -u serabot -- /opt/serabot/serabot \
     --config /etc/serabot/serabot.toml secrets set onedrive_client_id
   ```

2. Export the MSAL refresh token from the MCP token cache into SeraBot (run in
   `/home/ubuntu/onedrive-mcp` with the MCP venv; the script prints only to
   stdout for piping):

   ```bash
   cd /home/ubuntu/onedrive-mcp
   ./venv/bin/python - <<'PY' | runuser -u serabot -- /opt/serabot/serabot \
     --config /etc/serabot/serabot.toml secrets set onedrive_refresh_token
   import json, os
   from pathlib import Path
   env = Path(".env")
   cache_path = Path(".token_cache.bin")
   if env.exists():
       for line in env.read_text().splitlines():
           if line.strip() and not line.startswith("#") and "=" in line:
               k, v = line.split("=", 1)
               os.environ.setdefault(k.strip(), v.strip())
   cache_path = Path(os.environ.get("TOKEN_CACHE_PATH", cache_path))
   data = json.loads(cache_path.read_text())
   refresh = None
   for entry in (data.get("RefreshToken") or {}).values():
       secret = entry.get("secret")
       if secret:
           refresh = secret
           break
   if not refresh:
       raise SystemExit("no refresh token in MSAL cache; run: python server.py auth")
   print(refresh, end="")
   PY
   ```

3. Set `[onedrive] enabled = true` and `tenant = "consumers"` in
   `/etc/serabot/serabot.toml`, then restart **only** `serabot.service`.

Owner verification (after approval-capable bot, e.g. maker):

- `onedrive_list` with no args lists `/SeraBot` (or your configured folder).
- `onedrive_upload` remains approval-gated; confirm the approval shows the
  destination path and artifact hash, then check the file in OneDrive.

## Verification status

Tests use synthetic fixtures. Do not describe providers, Telegram, OneDrive, or
the desktop client as live until the owner has verified them against the real
account. The API alone is live on OVH.
