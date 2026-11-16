#!/usr/bin/env bash
# B7 load fixture (v0.50). Runs the real worker loop on a file SQLite database
# against a slow synthetic provider: one user queues a backlog of subagent turns
# and another user sends a single message. It passes when no task fails (no
# "database is locked") and the second user is not starved.
#
# Raise [runtime] max_concurrent_turns above 2 only after this passes on the
# target host with the worker count you intend to run (owner decision O4).
#
#   scripts/load-fixture.sh            # 4 workers, 16 children
#   LOAD_WORKERS=8 scripts/load-fixture.sh
#   LOAD_WORKERS=2 LOAD_CHILDREN=32 LOAD_DELAY_MS=300 scripts/load-fixture.sh
set -euo pipefail
cd "$(dirname "$0")/.."
export LOAD_WORKERS="${LOAD_WORKERS:-4}"
export LOAD_CHILDREN="${LOAD_CHILDREN:-16}"
export LOAD_DELAY_MS="${LOAD_DELAY_MS:-150}"
cargo test -p serabot --lib --locked --release -- --ignored --nocapture \
  load_fixture_children_do_not_starve_another_user_or_contend
