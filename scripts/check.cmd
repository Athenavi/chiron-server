@echo off
setlocal
call "%~dp0cargo-msvc.cmd" fmt --all --check || exit /b 1
call "%~dp0cargo-msvc.cmd" clippy --all-targets -- -D warnings || exit /b 1
call "%~dp0cargo-msvc.cmd" test --all || exit /b 1
