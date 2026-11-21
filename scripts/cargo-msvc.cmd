@echo off
setlocal
set "CARGO_EXE="
for /d %%D in ("D:\scoop\apps\rust-msvc\*") do (
  if exist "%%~fD\bin\cargo.exe" set "CARGO_EXE=%%~fD\bin\cargo.exe"
)
if not defined CARGO_EXE (
  echo cargo.exe not found under D:\scoop\apps\rust-msvc
  exit /b 1
)
for %%P in ("%CARGO_EXE%") do set "RUST_BIN=%%~dpP"
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul
set "PATH=%RUST_BIN%;%PATH%"
"%CARGO_EXE%" %*
