@echo off
rem B7 load fixture (v0.50). Windows wrapper for scripts\load-fixture.sh.
rem   scripts\load-fixture.bat            4 workers, 16 children
rem   set LOAD_WORKERS=8 && scripts\load-fixture.bat
if "%LOAD_WORKERS%"=="" set LOAD_WORKERS=4
if "%LOAD_CHILDREN%"=="" set LOAD_CHILDREN=16
if "%LOAD_DELAY_MS%"=="" set LOAD_DELAY_MS=150
call "%~dp0cargo-win.bat" test -p serabot --lib --locked --release -- --ignored --nocapture load_fixture_children_do_not_starve_another_user_or_contend
