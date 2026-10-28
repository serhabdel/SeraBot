#!/usr/bin/env bash
# Additive installer for the SeraBot bundle. Run only with explicit owner approval.
# It never edits, stops, or removes existing services, containers, or data.
set -euo pipefail

bundle_dir="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
service_user="serabot"
opt_dir="/opt/serabot"
etc_dir="/etc/serabot"
data_dir="/var/lib/serabot"

[[ "${EUID}" -eq 0 ]] || { echo "run as root after approval" >&2; exit 1; }
[[ -x "$bundle_dir/serabot" ]] || { echo "missing release binary: $bundle_dir/serabot" >&2; exit 1; }
[[ -f "$bundle_dir/serabot.toml.example" ]] || { echo "missing config example" >&2; exit 1; }
service_file="$bundle_dir/deploy/serabot.service"
[[ -f "$service_file" ]] || service_file="$bundle_dir/serabot.service"
[[ -f "$service_file" ]] || { echo "missing systemd unit" >&2; exit 1; }

if id -u "$service_user" >/dev/null 2>&1; then
  echo "user $service_user already exists; preserving it"
else
  useradd --system --home-dir "$data_dir" --shell /usr/sbin/nologin "$service_user"
fi

install -d -m 0755 "$opt_dir" "$etc_dir" "$etc_dir/config" "$etc_dir/bots" "$etc_dir/prompts"
install -d -m 0750 -o "$service_user" -g "$service_user" "$data_dir" "$data_dir/inbox" "$data_dir/artifacts" "$data_dir/models"
install -d -m 0700 -o "$service_user" -g "$service_user" "$etc_dir/private"

install -m 0755 "$bundle_dir/serabot" "$opt_dir/serabot"
# Refuse to replace owner-edited configuration or the service definition.
cp -n "$bundle_dir/serabot.toml.example" "$etc_dir/serabot.toml" || true
cp -R -n "$bundle_dir/bots/." "$etc_dir/bots/" || true
cp -R -n "$bundle_dir/prompts/." "$etc_dir/prompts/" || true
cp -R -n "$bundle_dir/config/." "$etc_dir/config/" || true
cp -n "$service_file" "/etc/systemd/system/serabot.service" || true

cat <<'EOF'
Bundle installed additively. Before starting:
1. Create /etc/serabot/config/brand.toml from brand.example.toml with approved branding.
2. Create /etc/serabot/private/blocklist.txt with the private policy.
3. Initialize secrets as the service user:
   runuser -u serabot -- /opt/serabot/serabot --config /etc/serabot/serabot.toml secrets init
   printf '%s' "$LLM_KEY" | runuser -u serabot -- /opt/serabot/serabot --config /etc/serabot/serabot.toml secrets set llm_api_key
4. Set the API token hash, owner Telegram IDs, and data-egress consent.
5. Provision the model in /var/lib/serabot/models by running the embedding self-test as serabot.
6. Ensure the API remains on loopback and terminate TLS in the existing reverse proxy/tunnel.
7. Only then: systemctl daemon-reload && systemctl enable --now serabot
EOF
