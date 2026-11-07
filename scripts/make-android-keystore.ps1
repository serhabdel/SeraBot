<#
.SYNOPSIS
  Creates the SeraBot Android release keystore OUTSIDE the repo and prints the
  `gh secret set` commands to store it in GitHub Actions.
.PARAMETER KeystorePath
  Where to create the .jks file (must not be inside a git repository).
.PARAMETER Alias
  Key alias (default: serabot).
.EXAMPLE
  .\scripts\make-android-keystore.ps1 -KeystorePath "$env:USERPROFILE\secrets\serabot-release.jks"
.NOTES
  Back the keystore up safely. If it is lost, installed apps can never be
  updated in place. Passwords are generated randomly and written only to a
  sibling .env file (next to the keystore), never printed.
#>
param(
  [Parameter(Mandatory = $true)][string]$KeystorePath,
  [string]$Alias = 'serabot',
  [string]$Repo = 'serhabdel/SeraBot'
)
$ErrorActionPreference = 'Stop'

$keystore = [IO.Path]::GetFullPath($KeystorePath)
if (Test-Path $keystore) { throw "Refusing to overwrite existing $keystore" }
$dir = Split-Path $keystore -Parent
New-Item -ItemType Directory -Force -Path $dir | Out-Null

# Refuse a location inside any git work tree (the repo is public).
$git = Get-Command git -ErrorAction SilentlyContinue
if ($git) {
  $inside = & git -C $dir rev-parse --is-inside-work-tree 2>$null
  if ($LASTEXITCODE -eq 0 -and $inside -eq 'true') {
    throw "KeystorePath must be outside any git repository: $dir"
  }
}
$keytool = Get-Command keytool -ErrorAction SilentlyContinue
if (-not $keytool) { throw 'keytool not found. Install a JDK (17+) or add it to PATH.' }

function New-Secret {
  $bytes = New-Object byte[] 24
  $rng = [Security.Cryptography.RandomNumberGenerator]::Create(); $rng.GetBytes($bytes); $rng.Dispose()
  ([Convert]::ToBase64String($bytes)).Replace('+', 'A').Replace('/', 'B').Replace('=', 'C')
}
$storePass = New-Secret
$keyPass = $storePass   # PKCS12 uses one password for store and key

& keytool -genkeypair -v -storetype PKCS12 -keystore $keystore -alias $Alias `
  -keyalg RSA -keysize 4096 -validity 10000 `
  -storepass $storePass -keypass $keyPass `
  -dname 'CN=SeraBot, O=SeraBot' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'keytool failed' }

$envFile = [IO.Path]::ChangeExtension($keystore, '.env')
@(
  "ANDROID_KEYSTORE_PASSWORD=$storePass"
  "ANDROID_KEY_ALIAS=$Alias"
  "ANDROID_KEY_PASSWORD=$keyPass"
) | Set-Content -Path $envFile -Encoding ascii
# Restrict both files to the current user.
foreach ($f in @($keystore, $envFile)) {
  icacls $f /inheritance:r /grant:r "$($env:USERNAME):(R,W)" | Out-Null
}

Write-Host "Keystore: $keystore"
Write-Host "Secrets file (passwords, keep private): $envFile"
Write-Host ''
Write-Host 'Run these in PowerShell (values are read from the files, not shown):'
Write-Host ''
Write-Host "[Convert]::ToBase64String([IO.File]::ReadAllBytes('$keystore')) | gh secret set ANDROID_KEYSTORE_BASE64 --repo $Repo"
Write-Host "(Get-Content '$envFile' | Where-Object { `$_ -like 'ANDROID_KEYSTORE_PASSWORD=*' }).Split('=',2)[1] | gh secret set ANDROID_KEYSTORE_PASSWORD --repo $Repo"
Write-Host "(Get-Content '$envFile' | Where-Object { `$_ -like 'ANDROID_KEY_ALIAS=*' }).Split('=',2)[1] | gh secret set ANDROID_KEY_ALIAS --repo $Repo"
Write-Host "(Get-Content '$envFile' | Where-Object { `$_ -like 'ANDROID_KEY_PASSWORD=*' }).Split('=',2)[1] | gh secret set ANDROID_KEY_PASSWORD --repo $Repo"
Write-Host ''
Write-Host 'Then back up the .jks and .env somewhere safe (password manager / offline).'

