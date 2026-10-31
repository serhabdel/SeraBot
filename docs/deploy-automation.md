# Automated OVH deploy (GitHub Actions + OIDC)

`deploy-ovh.yml` deploys a release tag to the OVH VPS without any long-lived
SSH key in GitHub. After a version bump on `main` passes CI, `ci.yml` creates
tag `vX.Y.Z`, dispatches `release.yml` (client downloads), and dispatches
`deploy-ovh.yml --ref <tag>` (server deploy). The deploy can also be run
manually (`workflow_dispatch`, input `tag`). The `deploy` job runs in the
protected **`ovh-deploy` environment with the owner as a required reviewer** —
approving the run is the remote-change authorization required by the
deployment policy. The exact remote manifest (`deploy/deploy.sh`) is printed
in the run log before the deploy.

To make deploys fully unattended instead, remove the required reviewer from
the `ovh-deploy` environment *and* enable branch protection on `main` with the
CI checks required; `deploy.sh` then auto-rolls back if `/healthz` does not
report the new build.

## How auth works

1. The `build` job runs on pinned `ubuntu-24.04` (the VPS runs 24.04 /
   glibc 2.39), builds `-p serabot --locked` with `SERABOT_RELEASES_REPO` and
   `SERABOT_BUILD_SHA=<tag sha>`, **fails if the binary needs
   `GLIBC_2.x > 2.39`**, and attaches `serabot-linux-x64` plus a dedicated
   `serabot-linux-x64.sha256` asset to the GitHub Release (the shared
   `SHA256SUMS` belongs to the client assets and is rewritten by
   `release.yml`, so the server binary uses its own checksum asset).
2. The `deploy` job requests a GitHub OIDC token (`audience=serabot-deploy`)
   and POSTs it with a freshly generated ephemeral ed25519 pubkey to the VPS
   broker `https://<vps>:8788/sign`.
3. The broker (`deploy/deploy-ca.py`) verifies the JWT against GitHub's JWKS
   and requires: `iss=https://token.actions.githubusercontent.com`,
   `aud=serabot-deploy`, `repository=serhabdel/SeraBot`,
   `workflow=.github/workflows/deploy-ovh.yml`, `environment=ovh-deploy`,
   `event_name=workflow_dispatch`, `ref=refs/tags/v*`. It then signs the
   pubkey with the deploy CA for principal `deploy`, valid **10 minutes**,
   with `force-command=sudo /opt/serabot-deploy/deploy.sh` and all other
   extensions cleared (no pty/forwarding/user-rc). It records the verified
   tag and commit sha in `/opt/serabot-deploy/pending-{tag,sha}`.
4. The runner SSHes to the **deploy-only sshd on port 2222** (its own
   instance; the existing sshd on 22 is untouched) with the cert. The
   forced command runs `deploy.sh`, which downloads the release assets,
   verifies `serabot-linux-x64.sha256`, re-checks host glibc, stages, backs up, swaps
   `/opt/serabot/serabot`, restarts **only `serabot.service`**, verifies
   `/healthz` reports the deployed sha, and rolls back automatically on
   failure. Host key is pinned via `deploy/known_hosts.deploy`.

Repo variables needed: `OVH_VPS_IP` (repository variable, not a secret).
No secrets are needed at all — that is the point of OIDC.

## One-time VPS setup (additive; nothing existing is modified or restarted)

Present this manifest to the owner before running it:

```text
NEW  user deploy (shell /bin/bash, no password)          # cert-only login
NEW  user deployca                                        # runs the CA broker
NEW  /opt/serabot-deploy/                        0755 root:root
NEW  /opt/serabot-deploy/deploy-ca.py            0644 root:root   (deploy/deploy-ca.py)
NEW  /opt/serabot-deploy/deploy.sh               0755 root:root   (deploy/deploy.sh)
NEW  /opt/serabot-deploy/sshd_config             0644 root:root   (deploy/sshd_config.deploy)
NEW  /opt/serabot-deploy/ca_key + ca_key.pub     0600/0644 deployca (ssh-keygen -t ed25519)
NEW  /opt/serabot-deploy/pending-tag, pending-sha     writable by deployca
NEW  /etc/systemd/system/serabot-deploy-ca.service    (deploy/serabot-deploy-ca.service)
NEW  /etc/systemd/system/serabot-deploy-sshd.service  (deploy/serabot-deploy-sshd.service)
NEW  /etc/sudoers.d/serabot-deploy                0440 root:root  (deploy runs only deploy.sh)
RUN  systemctl daemon-reload && systemctl enable --now serabot-deploy-ca serabot-deploy-sshd
RUN  ufw allow 2222/tcp   # deploy sshd
RUN  ufw allow 8788/tcp   # CA broker (JWT is the only auth; rate-limited)
RUN  ssh-keyscan -p 2222 <VPS_IP> > deploy/known_hosts.deploy   (commit the pin)
UNTOUCHED: sshd:22 (no reload/restart), serabot.service, cloudflared.service,
           cloudflared-bot.service, docker, onedrive-mcp.service,
           /etc/serabot, /var/lib/serabot (except files deploy.sh writes)
```

Setup commands:

```bash
sudo useradd -m -s /bin/bash deploy
# sshd refuses a locked account even when the login is certificate-only, and
# useradd leaves the password field as "!". "*" means "no password" without
# being locked, so password auth stays impossible while cert auth works.
sudo usermod -p '*' deploy
sudo useradd -r -M -s /usr/sbin/nologin deployca
sudo install -d -m 0755 /opt/serabot-deploy
sudo install -m 0644 deploy/deploy-ca.py deploy/deploy.sh deploy/sshd_config.deploy /opt/serabot-deploy/
sudo chmod 0755 /opt/serabot-deploy/deploy.sh
# The directory is root-owned, so generate the CA key as root and hand it to
# deployca; `sudo -u deployca ssh-keygen` cannot create files there.
sudo ssh-keygen -t ed25519 -N "" -f /opt/serabot-deploy/ca_key -q
sudo chown deployca:deployca /opt/serabot-deploy/ca_key /opt/serabot-deploy/ca_key.pub
sudo chmod 0600 /opt/serabot-deploy/ca_key
sudo install -m 0644 deploy/serabot-deploy-ca.service deploy/serabot-deploy-sshd.service /etc/systemd/system/
# The forced command is `sudo /opt/serabot-deploy/deploy.sh`; without this rule
# the deploy user cannot run it and every deploy fails at the forced command.
printf 'deploy ALL=(root) NOPASSWD: /opt/serabot-deploy/deploy.sh\n' | sudo tee /etc/sudoers.d/serabot-deploy
sudo chmod 0440 /etc/sudoers.d/serabot-deploy
sudo visudo -c
sudo touch /opt/serabot-deploy/pending-tag /opt/serabot-deploy/pending-sha
sudo chown deployca:deployca /opt/serabot-deploy/pending-tag /opt/serabot-deploy/pending-sha
sudo systemctl daemon-reload
sudo systemctl enable --now serabot-deploy-ca serabot-deploy-sshd
sudo ufw allow 2222/tcp
sudo ufw allow 8788/tcp
ssh-keyscan -p 2222 <VPS_IP>   # pin into deploy/known_hosts.deploy
```

### Known limitation: the CA broker speaks plain HTTP

`deploy-ca.py` binds `0.0.0.0:8788` with `ThreadingHTTPServer`, so the OIDC
token crosses the network in cleartext and the workflow must call
`http://<VPS_IP>:8788/sign`. The token is audience-scoped (`serabot-deploy`),
single-use and short-lived, and the certificate it mints lives 10 minutes and
carries a forced command — so the blast radius of interception is a forced
redeploy of a legitimate release, not arbitrary access. Hardening this means
terminating TLS on 8788 with a certificate the runner pins via `--cacert`;
that is a deliberate follow-up, not an oversight.

## Deploy a release

Automatic: bump `version` in `Cargo.toml` on `main`, push — CI tests, then
creates `v<version>`, publishes client assets, and dispatches `deploy-ovh`
for the server; approve the `ovh-deploy` environment prompt when it appears.
Manual: GitHub → Actions → deploy-ovh → Run workflow → tag → approve.
Verify: `bot.seradyx.tech/healthz` shows the new build sha; the run's last
step prints healthz + the CIMD route.

Rollback is automatic on verification failure; manually:
`cp /var/lib/serabot/releases/serabot-pre-<tag> /opt/serabot/serabot && systemctl restart serabot`.
