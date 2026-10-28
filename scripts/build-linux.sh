#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
cargo build --release --locked

package_root="dist/serabot-linux-x86_64"
rm -rf "$package_root"
mkdir -p "$package_root"
install -m 0755 target/release/serabot "$package_root/serabot"
cp serabot.toml "$package_root/serabot.toml.example"
cp LICENSE "$package_root/LICENSE"
cp -R bots prompts deploy "$package_root/"
mkdir -p "$package_root/config"
cp config/*.example.toml "$package_root/config/"
chmod +x "$package_root/deploy/install.sh"

tar -C dist -czf dist/serabot-linux-x86_64.tar.gz "serabot-linux-x86_64"
sha256sum dist/serabot-linux-x86_64.tar.gz > dist/serabot-linux-x86_64.tar.gz.sha256
file "$package_root/serabot"
ls -lh dist/serabot-linux-x86_64.tar.gz
