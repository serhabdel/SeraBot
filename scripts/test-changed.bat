@echo off
rem Thin wrapper: all logic lives in test-changed.ps1 (quoting filtersets in cmd is pain).
rem Translate GNU-style --args to PowerShell -Args (delayed expansion: %ARGS% inside a
rem parenthesized block would expand at parse time and lose the earlier replacements).
setlocal enabledelayedexpansion
set "ARGS=%*"
set "ARGS=!ARGS:--dry-run=-DryRun!"
set "ARGS=!ARGS:--full=-Full!"
set "ARGS=!ARGS:--base=-Base!"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0test-changed.ps1" !ARGS!
exit /b %errorlevel%
