@echo off
rem Start the Rust GPUI Desktop app (cst-pilot) directly inside the workspace.
rem Keep this entry point ASCII.
setlocal DisableDelayedExpansion
set "ROOT=%~dp0"
chcp 65001 >nul

set "CARGO_CMD="
where cargo >nul 2>nul
if not errorlevel 1 set "CARGO_CMD=cargo"
if not defined CARGO_CMD if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "CARGO_CMD=%USERPROFILE%\.cargo\bin\cargo.exe"

if /I "%~1"=="--release" (
    shift
    goto :build_release
)
if /I "%~1"=="--build" (
    shift
    goto :build_debug
)

if exist "%ROOT%target\release\cst-pilot.exe" if exist "%ROOT%target\release\cst-server.exe" if exist "%ROOT%target\release\cst-updater.exe" goto :run_release
if exist "%ROOT%target\debug\cst-pilot.exe" if exist "%ROOT%target\debug\cst-server.exe" if exist "%ROOT%target\debug\cst-updater.exe" goto :run_debug
goto :build_debug

:build_release
if not defined CARGO_CMD goto :binary_missing
"%CARGO_CMD%" build --manifest-path "%ROOT%Cargo.toml" --release -p cst-desktop -p cst-server -p cst-updater --locked
if errorlevel 1 exit /b %errorlevel%
goto :run_release

:build_debug
if not defined CARGO_CMD goto :binary_missing
"%CARGO_CMD%" build --manifest-path "%ROOT%Cargo.toml" -p cst-desktop -p cst-server -p cst-updater --locked
if errorlevel 1 exit /b %errorlevel%
goto :run_debug

:run_release
"%ROOT%target\release\cst-pilot.exe" --kit-root "%ROOT%." %1 %2 %3 %4 %5 %6 %7 %8 %9
exit /b %errorlevel%

:run_debug
"%ROOT%target\debug\cst-pilot.exe" --kit-root "%ROOT%." %1 %2 %3 %4 %5 %6 %7 %8 %9
exit /b %errorlevel%

:binary_missing
echo [cst-pilot] ERROR: desktop binaries were not found under target\ and cargo is not available. 1>&2
exit /b 1
