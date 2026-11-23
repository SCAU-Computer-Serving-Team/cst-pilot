@echo off
rem Start the Rust Web backend (cst-server) in standalone mode and open the browser.
rem Dev repo isolates runtime user data under .\data\ (gitignored).
rem Keep this entry point ASCII.
setlocal DisableDelayedExpansion
set "ROOT=%~dp0"
chcp 65001 >nul

set "CST_PILOT_ROOT=%ROOT%."

if exist "%ROOT%target\release\cst-server.exe" goto :run_release
if exist "%ROOT%target\debug\cst-server.exe" goto :run_debug

where cargo >nul 2>nul
if not errorlevel 1 goto :run_cargo
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" goto :run_user_cargo
goto :binary_missing

:run_release
"%ROOT%target\release\cst-server.exe" --standalone-web --open-browser --kit-root "%ROOT%." %*
exit /b %errorlevel%

:run_debug
"%ROOT%target\debug\cst-server.exe" --standalone-web --open-browser --kit-root "%ROOT%." %*
exit /b %errorlevel%

:run_cargo
cargo run --manifest-path "%ROOT%Cargo.toml" -p cst-server --bin cst-server -- --standalone-web --open-browser --kit-root "%ROOT%." %*
exit /b %errorlevel%

:run_user_cargo
"%USERPROFILE%\.cargo\bin\cargo.exe" run --manifest-path "%ROOT%Cargo.toml" -p cst-server --bin cst-server -- --standalone-web --open-browser --kit-root "%ROOT%." %*
exit /b %errorlevel%

:binary_missing
echo [cst-pilot] ERROR: cst-server.exe was not found under target\ and cargo is not available. 1>&2
exit /b 1
