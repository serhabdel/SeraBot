#!/usr/bin/env bash
# Linux gate for the multi-user reconstruction (run inside WSL).
# Usage: bash scripts/gate-wsl.sh /path/to/checkout
set -uo pipefail

SOURCE="${1:-/tmp/serabot-gate}"
source "$HOME/.cargo/env" 2>/dev/null || true
cd "$SOURCE" || exit 1

status=0

echo "=== cargo fmt --all -- --check ==="
cargo fmt --all -- --check || status=1

echo "=== cargo clippy --workspace --all-targets -- -D warnings ==="
cargo clippy --workspace --all-targets -- -D warnings || status=1

echo "=== cargo test --workspace --locked ==="
cargo test --workspace --locked || status=1

echo "=== cargo build --release --locked ==="
cargo build --release --locked || status=1

echo "=== gate status: $status ==="
exit "$status"
