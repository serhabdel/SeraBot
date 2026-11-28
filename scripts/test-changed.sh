#!/usr/bin/env bash
# L1 fast test loop: run only the tests affected by files changed vs a base ref.
#
# Mapping (nextest filtersets have no file() predicate, so we map to test names):
#   crates/<pkg>/src/<mod>.rs   -> test(/^<mod with / as ::>::/)   (inline #[cfg(test)])
#   crates/<pkg>/src/lib.rs|main.rs -> package(<pkg>)               (too central, run crate)
#   crates/<pkg>/tests/<n>.rs   -> binary(<n>)                       (integration tests)
#   other files in crates/<pkg> -> package(<pkg>)
#
# Usage:
#   bash scripts/test-changed.sh                 # diff vs origin/main...HEAD
#   bash scripts/test-changed.sh --base <ref>    # diff vs another ref
#   bash scripts/test-changed.sh --full          # full serabot suite (L2)
#   bash scripts/test-changed.sh --dry-run       # print the nextest command, don't run
#
# Requires: cargo-nextest (cargo install cargo-nextest --locked)
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

BASE="origin/main...HEAD"
MODE="changed"
DRY_RUN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE="${2:?--base needs a ref}"; shift 2 ;;
    --full) MODE="full"; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if ! command -v cargo-nextest >/dev/null 2>&1; then
  echo "cargo-nextest not found. Install with: cargo install cargo-nextest --locked" >&2
  exit 1
fi

file_to_pred() {
  local f="$1" pkg rest mod name
  case "$f" in
    crates/*/src/*.rs)
      pkg="${f#crates/}"; pkg="${pkg%%/*}"
      rest="${f#crates/$pkg/src/}"
      case "$rest" in
        lib.rs|main.rs) echo "package($pkg)"; return ;;
      esac
      mod="${rest%.rs}"
      mod="${mod//\//::}"
      case "$mod" in
        *::mod) mod="${mod%::mod}" ;;
        mod) mod="" ;;
      esac
      if [ -z "$mod" ]; then echo "package($pkg)"; else echo "test(/^${mod}::/)"; fi
      ;;
    crates/*/tests/*.rs)
      name="$(basename "$f" .rs)"
      echo "binary($name)"
      ;;
    crates/*)
      pkg="${f#crates/}"; pkg="${pkg%%/*}"
      echo "package($pkg)"
      ;;
    *) return 1 ;;
  esac
}

if [ "$MODE" = "full" ]; then
  CMD=(cargo nextest run -p serabot --locked)
else
  mapfile -t FILES < <(git diff --name-only --diff-filter=ACMR "$BASE")
  EXPR=""
  N=0
  for f in "${FILES[@]}"; do
    p="$(file_to_pred "$f")" || continue
    [ -n "$EXPR" ] && EXPR="$EXPR or "
    EXPR="${EXPR}${p}"
    N=$((N+1))
  done
  if [ -z "$EXPR" ]; then
    echo "No mappable changed files vs $BASE - nothing to run."
    exit 0
  fi
  echo "Mapped $N changed files to tests."
  CMD=(cargo nextest run --locked -E "$EXPR")
fi

if [ "$DRY_RUN" -eq 1 ]; then
  printf '%q ' "${CMD[@]}"
  echo
  exit 0
fi

exec "${CMD[@]}"
