@echo off
rem CST Pilot: request Windows UAC authorization before starting the portable runtime.
rem Keep this entry point ASCII; startup policy lives in launcher.ps1.
setlocal DisableDelayedExpansion
set "ROOT=%~dp0"
if not exist "%ROOT%pwsh\pwsh.exe" goto :pwsh_missing
set "POWERSHELL_TELEMETRY_OPTOUT=1"
set "POWERSHELL_UPDATECHECK=Off"
chcp 65001 >nul
"%ROOT%pwsh\pwsh.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%ROOT%launcher.ps1" %*
exit /b %errorlevel%

:pwsh_missing
echo [cst-pilot] ERROR: bundled pwsh\pwsh.exe is missing. 1>&2
exit /b 1
