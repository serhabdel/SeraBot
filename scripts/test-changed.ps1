# L1 fast test loop (Windows): run only the tests affected by files changed vs a base ref.
#
# Mapping (nextest filtersets have no file() predicate, so we map to test names):
#   crates/<pkg>/src/<mod>.rs   -> test(/^<mod with / as ::>::/)   (inline #[cfg(test)])
#   crates/<pkg>/src/lib.rs|main.rs -> package(<pkg>)               (too central, run crate)
#   crates/<pkg>/tests/<n>.rs   -> binary(<n>)                       (integration tests)
#   other files in crates/<pkg> -> package(<pkg>)
#
# Usage (via test-changed.bat, which translates --args to -Args):
#   scripts\test-changed.bat                  # diff vs origin/main...HEAD
#   scripts\test-changed.bat -Base <ref>      # diff vs another ref
#   scripts\test-changed.bat -Full            # full serabot suite (L2)
#   scripts\test-changed.bat -DryRun          # print the nextest command, don't run
#
# Requires: cargo-nextest (cargo install cargo-nextest --locked)
param(
  [string]$Base = "origin/main...HEAD",
  [switch]$Full,
  [switch]$DryRun
)

$ErrorActionPreference = "Stop"

function ConvertTo-TestPredicate {
  param([string]$Path)
  if ($Path -match '^crates/([^/]+)/src/(.+\.rs)$') {
    $pkg = $Matches[1]
    $rest = $Matches[2]
    if ($rest -eq "lib.rs" -or $rest -eq "main.rs") { return "package($pkg)" }
    $mod = $rest -replace '\.rs$', '' -replace '/', '::'
    if ($mod.EndsWith('::mod')) { $mod = $mod.Substring(0, $mod.Length - 5) }
    elseif ($mod -eq 'mod') { $mod = '' }
    if ($mod -eq '') { return "package($pkg)" }
    return "test(/^${mod}::/)"
  }
  if ($Path -match '^crates/[^/]+/tests/([^/]+)\.rs$') {
    return "binary($($Matches[1]))"
  }
  if ($Path -match '^crates/([^/]+)/') {
    return "package($($Matches[1]))"
  }
  return $null
}

Push-Location (Join-Path $PSScriptRoot "..")
try {
  if (-not (Get-Command cargo-nextest -ErrorAction SilentlyContinue)) {
    Write-Error "cargo-nextest not found. Install with: cargo install cargo-nextest --locked"
    exit 1
  }

  if ($Full) {
    $cmd = @("cargo", "nextest", "run", "-p", "serabot", "--locked")
  } else {
    $files = git diff --name-only --diff-filter=ACMR $Base
    $preds = @()
    foreach ($f in $files) {
      $p = ConvertTo-TestPredicate $f
      if ($p) { $preds += $p }
    }
    if ($preds.Count -eq 0) {
      Write-Output "No mappable changed files vs $Base - nothing to run."
      exit 0
    }
    Write-Output "Mapped $($preds.Count) changed files to tests."
    # Filterset DSL has no embedded quotes, so the expression passes through as-is.
    $cmd = @("cargo", "nextest", "run", "--locked", "-E", ($preds -join " or "))
  }

  if ($DryRun) {
    Write-Output ($cmd -join " ")
    exit 0
  }

  & $cmd[0] $cmd[1..($cmd.Count - 1)]
  exit $LASTEXITCODE
} finally {
  Pop-Location
}
