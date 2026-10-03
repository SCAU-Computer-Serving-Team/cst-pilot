@echo off
rem ============================================================
rem  CST Pilot launcher v10 (0.3 layout + portable program state)
rem  cst-pilot: Computer Service Team portable diagnostics kit,
rem  built on pi coding agent. 'pi' kept in the name in tribute.
rem  Layout:
rem    agent\.runtime\prx.bin = official pi binary, renamed + hidden
rem      (never exposed as pi.exe at the root; not a real sandbox)
rem    node\    = Node.js runtime (dev checkout fallback, 0.2 layout)
rem    pwsh\    = PowerShell 7 runtime
rem    wiztree\ = WizTree portable (fast disk usage, admin only)
rem  Isolation:
rem    - PI_CODING_AGENT_DIR unconditionally overridden -> agent\home
rem    - STRICT PATH whitelist (host PATH ignored)
rem    - Temporary state under .state\; pi config/sessions in agent\home:
rem        TMP/TEMP       -> .state\tmp   (jiti cache, pi logs, PDF out)
rem        XDG_CACHE_HOME -> .state\cache (fff host-cache redirect)
rem        FFF_*_DB       -> .state\data  (fff frecency/history)
rem        PSModuleAnalysisCachePath -> .state\cache (PS7 module cache;
rem          its LOCALAPPDATA default is NOT covered by XDG_CACHE_HOME)
rem    - Built-in pi telemetry off; CST telemetry is separate:
rem        PI_OFFLINE=1 (pi: no startup network ops, no install/update
rem        telemetry, no version check), enableInstallTelemetry=false
rem        in settings.json. CST telemetry (extension, own config at
rem        agent\home\telemetry.json) is NOT blocked by PI_OFFLINE
rem        POWERSHELL_TELEMETRY_OPTOUT=1, POWERSHELL_UPDATECHECK=Off
rem    - --no-skills + explicit --skill (only agent\home\skills)
rem    - --no-context-files, defaultProjectTrust=never (settings)
rem    - UTF-8 console (chcp 65001) + PYTHONUTF8 injection
rem  Engine selection: agent\.runtime\prx.bin when present (0.3 release),
rem  otherwise the node-based dev checkout (0.2 layout, agent\node_modules).
rem  Host boundary: these settings are not an OS sandbox and cannot
rem  suppress Windows logging or every native runtime side effect.
rem    - Known limit: if client already has pwsh profile files, pwsh
rem      updates StartupProfileData-NonInteractive (JIT start profile,
rem      binary, no user data). Hardcoded in pwsh (.NET ProfileOpti-
rem      mization), no official switch (PowerShell issue #26528).
rem  NOTE: keep this file pure ASCII; cmd parses it as ANSI/GBK
rem ============================================================
setlocal
set "ROOT=%~dp0"
set "STATE=%ROOT%.state"

set "PI_CODING_AGENT_DIR=%ROOT%agent\home"
set "PI_OFFLINE=1"
set "POWERSHELL_TELEMETRY_OPTOUT=1"
set "POWERSHELL_UPDATECHECK=Off"

rem ---- create volatile state dirs; fail loudly if volume is
rem ---- read-only, write-protected or full
md "%STATE%\tmp" 2>nul
md "%STATE%\cache" 2>nul
md "%STATE%\data" 2>nul
if not exist "%STATE%\tmp\" goto :state_fail
if not exist "%STATE%\cache\" goto :state_fail
if not exist "%STATE%\data\" goto :state_fail
rem ---- copy sets a reliable exit status; echo redirection does not.
rem ---- Keep these small probes; no cleanup or host fallback needed.
copy /y "%~f0" "%STATE%\tmp\.write-probe" >nul 2>nul
if errorlevel 1 goto :state_fail
copy /y "%~f0" "%STATE%\cache\.write-probe" >nul 2>nul
if errorlevel 1 goto :state_fail
copy /y "%~f0" "%STATE%\data\.write-probe" >nul 2>nul
if errorlevel 1 goto :state_fail

rem ---- drop the WizTree config at every start. The ini holds per-host
rem ---- display state (DPI, window geometry, row and column sizes) and
rem ---- makes WizTree stall once the kit moves to another machine.
rem ---- Renamed, not deleted: a supporter code may live in the file, and
rem ---- WizTree rebuilds defaults on its next run. Fixed name, so the
rem ---- backup cannot pile up. A locked file makes this fail silently.
if exist "%ROOT%wiztree\WizTree3.ini" move /y "%ROOT%wiztree\WizTree3.ini" "%ROOT%wiztree\WizTree3.ini.bad" >nul 2>nul

set "TMP=%STATE%\tmp"
set "TEMP=%STATE%\tmp"
set "XDG_CACHE_HOME=%STATE%\cache"
set "XDG_DATA_HOME=%STATE%\data"
set "FFF_FRECENCY_DB=%STATE%\data\fff-frecency.mdb"
set "FFF_HISTORY_DB=%STATE%\data\fff-history.mdb"
set "PSModuleAnalysisCachePath=%STATE%\cache\PSModuleAnalysisCache.txt"

rem ---- strip host model-provider credentials and endpoint overrides: the
rem ---- kit's login state lives only in agent\home\auth.json. Inherited
rem ---- *_API_KEY / token variables would otherwise make providers look
rem ---- logged in; CSTOA_OA_HOST could redirect the OA authorization flow.
for %%V in (
    COPILOT_GITHUB_TOKEN
    ANTHROPIC_AUTH_TOKEN
    ANTHROPIC_OAUTH_TOKEN
    ANTHROPIC_API_KEY
    ANT_LING_API_KEY
    QWEN_TOKEN_PLAN_API_KEY
    QWEN_TOKEN_PLAN_CN_API_KEY
    OPENAI_API_KEY
    AZURE_OPENAI_API_KEY
    NVIDIA_API_KEY
    DEEPSEEK_API_KEY
    GEMINI_API_KEY
    GOOGLE_CLOUD_API_KEY
    GOOGLE_APPLICATION_CREDENTIALS
    GOOGLE_CLOUD_PROJECT
    GCLOUD_PROJECT
    GOOGLE_CLOUD_LOCATION
    GROQ_API_KEY
    CEREBRAS_API_KEY
    XAI_API_KEY
    RADIUS_API_KEY
    OPENROUTER_API_KEY
    AI_GATEWAY_API_KEY
    ZAI_API_KEY
    ZAI_CODING_CN_API_KEY
    MISTRAL_API_KEY
    MINIMAX_API_KEY
    MINIMAX_CN_API_KEY
    MOONSHOT_API_KEY
    HF_TOKEN
    FIREWORKS_API_KEY
    TOGETHER_API_KEY
    BASETEN_API_KEY
    OPENCODE_API_KEY
    KIMI_API_KEY
    META_API_KEY
    CLOUDFLARE_API_KEY
    XIAOMI_API_KEY
    XIAOMI_TOKEN_PLAN_CN_API_KEY
    XIAOMI_TOKEN_PLAN_AMS_API_KEY
    XIAOMI_TOKEN_PLAN_SGP_API_KEY
    AWS_PROFILE
    AWS_ACCESS_KEY_ID
    AWS_SECRET_ACCESS_KEY
    AWS_BEARER_TOKEN_BEDROCK
    AWS_CONTAINER_CREDENTIALS_RELATIVE_URI
    AWS_CONTAINER_CREDENTIALS_FULL_URI
    AWS_WEB_IDENTITY_TOKEN_FILE
    CSTOA_OA_HOST
) do set "%%V="

if defined PI_INHERIT_HOST_PATH (
    set "PATH=%ROOT%pwsh;%ROOT%node;%PATH%"
) else (
    set "PATH=%ROOT%pwsh;%ROOT%node;%WINDIR%\System32;%WINDIR%;%WINDIR%\System32\Wbem;%WINDIR%\System32\WindowsPowerShell\v1.0"
)

chcp 65001 >nul
set "PYTHONUTF8=1"
set "PYTHONIOENCODING=utf-8"

if exist "%ROOT%agent\.runtime\prx.bin" goto :run_binary
goto :run_node

:run_binary
attrib +h "%ROOT%agent\.runtime" >nul 2>&1
"%ROOT%agent\.runtime\prx.bin" --no-skills --skill "%ROOT%agent\home\skills" --no-context-files %*
endlocal
exit /b %errorlevel%

:run_node
"%ROOT%node\node.exe" "%ROOT%agent\node_modules\@earendil-works\pi-coding-agent\dist\bundle\cli.js" --no-skills --skill "%ROOT%agent\home\skills" --no-context-files %*
endlocal
exit /b %errorlevel%

:state_fail
echo [cst-pilot] ERROR: cannot create or write state directories. 1>&2
echo [cst-pilot] The media may be write-protected, read-only or full. 1>&2
echo [cst-pilot] Refusing to start without writable portable state. 1>&2
exit /b 1
