#!/usr/bin/env bash
set -euo pipefail

config="${1:-/etc/serabot/serabot.toml}"
backup_dir="${2:-/var/backups/serabot}"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
binary="${SERABOT_BIN:-/opt/serabot/serabot}"

mkdir -p "$backup_dir"
"$binary" --config "$config" backup "$backup_dir/serabot-$timestamp.db"

# Artifacts are reproducible from source, but retaining them preserves audit context.
if [[ -d /var/lib/serabot/artifacts ]]; then
  tar -C /var/lib/serabot -czf "$backup_dir/serabot-artifacts-$timestamp.tar.gz" artifacts
fi

echo "backup complete: $backup_dir/serabot-$timestamp.db"
