#!/bin/bash
# SeraBot OVH deploy script. Runs as root via the forced command on the
# deploy-only sshd (port 2222): an SSH cert minted by deploy-ca.py from a
# verified GitHub OIDC token (environment ovh-deploy, required reviewer) is
# the only way in, and this script is the only command it can run.
#
# Exact manifest (nothing else is touched):
#   NEW   /var/lib/serabot/releases/serabot-<tag>-<sha>     (staged binary)
#   NEW   /var/lib/serabot/releases/serabot-pre-<tag>       (backup of old binary)
#   NEW   /var/lib/serabot/backups/serabot-db-pre-<tag>.db (SQLite online backup)
#   REPL  /opt/serabot/serabot                              (after SHA-256 + glibc check)
#   RUN   systemctl restart serabot                         (only this service)
#   VERIFY healthz build == deployed sha; journal clean; else ROLLBACK.
#   UNTOUCHED: sshd:22, cloudflared*, docker, onedrive-mcp, /etc/serabot.
set -euo pipefail

DEPLOY_DIR=/opt/serabot-deploy
TAG=$(cat "$DEPLOY_DIR/pending-tag")
SHA=$(cat "$DEPLOY_DIR/pending-sha")
REPO=serhabdel/SeraBot
SHORT=${SHA:0:7}
STAGED=/var/lib/serabot/releases/serabot-${TAG}-${SHORT}
BACKUP=/var/lib/serabot/releases/serabot-pre-${TAG}
DBBACKUP=/var/lib/serabot/backups/serabot-db-pre-${TAG}.db
ASSET_URL="https://github.com/${REPO}/releases/download/${TAG}"

echo "deploy: tag=${TAG} sha=${SHA}"

echo "== preflight =="
systemctl is-active serabot
df -h /
ldd --version | head -1

echo "== download release assets and verify SHA256SUMS =="
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
curl -fsSL "${ASSET_URL}/serabot-linux-x64" -o "$TMP/serabot-linux-x64"
curl -fsSL "${ASSET_URL}/SHA256SUMS" -o "$TMP/SHA256SUMS"
(cd "$TMP" && sha256sum -c SHA256SUMS --ignore-missing --quiet)
EXPECTED=$(grep 'serabot-linux-x64$' "$TMP/SHA256SUMS" | cut -d' ' -f1)
ACTUAL=$(sha256sum "$TMP/serabot-linux-x64" | cut -d' ' -f1)
[ "$EXPECTED" = "$ACTUAL" ] || { echo "SHA mismatch"; exit 1; }

echo "== host-side glibc check =="
MAXGLIBC=$(objdump -T "$TMP/serabot-linux-x64" | grep -o 'GLIBC_2\.[0-9]*' | sort -uV | tail -1)
echo "binary needs ${MAXGLIBC}"
[ "$MAXGLIBC" \< "GLIBC_2.40" ] || { echo "glibc too new for this host"; exit 1; }

echo "== stage =="
install -m 0755 "$TMP/serabot-linux-x64" "$STAGED"
[ "$(sha256sum "$STAGED" | cut -d' ' -f1)" = "$EXPECTED" ] || { echo "staged SHA mismatch"; exit 1; }

echo "== backups =="
cp -a /opt/serabot/serabot "$BACKUP"
sqlite3 /var/lib/serabot/serabot.db ".backup '${DBBACKUP}'"

echo "== swap + restart only serabot =="
cp "$STAGED" /opt/serabot/serabot
chmod 0755 /opt/serabot/serabot
systemctl restart serabot
sleep 4

verify() {
  systemctl is-active serabot || return 1
  curl -fsS http://127.0.0.1:8787/healthz | python3 -c '
import json, sys
d = json.load(sys.stdin)
sha = open("/opt/serabot-deploy/pending-sha").read().strip()
ok = d.get("ok") is True and d.get("build") == sha
print("healthz:", d, "expected build:", sha)
sys.exit(0 if ok else 1)
'
}

if verify; then
  echo "== deployed OK =="
  journalctl -u serabot -n 15 --no-pager
  exit 0
fi

echo "== VERIFY FAILED: rolling back =="
cp -a "$BACKUP" /opt/serabot/serabot
systemctl restart serabot
sleep 4
systemctl is-active serabot
journalctl -u serabot -n 15 --no-pager
exit 1
