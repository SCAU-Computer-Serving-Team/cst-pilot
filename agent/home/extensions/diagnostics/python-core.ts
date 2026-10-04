import { psString } from "./pwsh-data.ts";
import { asRecord, asRecords, createPwshRunner, PWSH } from "./runtime.ts";

export type PythonScope = "overview" | "installations" | "environments" | "config" | "all";

export interface PythonAuditInput {
	scope: PythonScope;
	path?: string;
	maxDepth: number;
	includeSizes: boolean;
}

// CST_PILOT_PWSH 仅供开发机注入系统 pwsh；产品环境使用工具包内置 pwsh。
const runPwsh = createPwshRunner({
	timeoutMs: 45_000,
	diagnostics: true,
	path: process.env.CST_PILOT_PWSH || PWSH,
});

const PYTHON_COMMAND = String.raw`
$ErrorActionPreference = 'SilentlyContinue'
$scanPath = __SCAN_PATH__
$maxDepth = __MAX_DEPTH__
$includeSizes = __INCLUDE_SIZES__

function Get-ToolVersion([string]$exe) {
  if (-not $exe -or -not (Test-Path -LiteralPath $exe)) { return $null }
  try {
    return ((& $exe --version 2>&1 | Select-Object -First 1 | Out-String).Trim())
  } catch { return $null }
}

function Get-EnvVersion([string]$dir) {
  $cfg = Join-Path $dir 'pyvenv.cfg'
  if (Test-Path -LiteralPath $cfg) {
    $line = Get-Content -LiteralPath $cfg -ErrorAction SilentlyContinue |
      Where-Object { $_ -match '^\s*version\s*=' } | Select-Object -First 1
    if ($line) { return (($line -split '=', 2)[1]).Trim() }
  }
  $python = Join-Path $dir 'python.exe'
  if (-not (Test-Path -LiteralPath $python)) { $python = Join-Path $dir 'Scripts\python.exe' }
  return Get-ToolVersion $python
}

function Get-EnvSize([string]$dir) {
  if (-not $includeSizes) { return $null }
  try {
    $sum = (Get-ChildItem -LiteralPath $dir -Recurse -Force -File -ErrorAction SilentlyContinue |
      Measure-Object -Property Length -Sum).Sum
    if ($null -eq $sum) { return 0 }
    return [int64]$sum
  } catch { return $null }
}

function Get-EnvKind([string]$dir) {
  if (Test-Path -LiteralPath (Join-Path $dir 'conda-meta\history')) { return 'conda' }
  if (Test-Path -LiteralPath (Join-Path $dir 'pyvenv.cfg')) {
    if ($dir -match '\\\.pyenv\\versions\\') { return 'pyenv' }
    if ($dir -match '\\pypoetry\\|\\poetry\\') { return 'poetry' }
    if ($dir -match '\\uv\\python\\|\\uv\\') { return 'uv' }
    return 'venv'
  }
  return 'unknown'
}

function Test-EnvDir([string]$dir) {
  if (-not $dir -or -not (Test-Path -LiteralPath $dir -PathType Container)) { return $false }
  if (Test-Path -LiteralPath (Join-Path $dir 'conda-meta\history')) { return $true }
  if (Test-Path -LiteralPath (Join-Path $dir 'pyvenv.cfg')) { return $true }
  if (Test-Path -LiteralPath (Join-Path $dir 'python.exe')) { return $true }
  if (Test-Path -LiteralPath (Join-Path $dir 'Scripts\python.exe')) { return $true }
  return $false
}

function Add-Environment([string]$dir, [string]$source) {
  if (-not (Test-EnvDir $dir)) { return }
  try { $full = [IO.Path]::GetFullPath($dir).TrimEnd('\') } catch { return }
  if ($seen.ContainsKey($full)) { return }
  $seen[$full] = $true
  $envs += [pscustomobject]@{
    path = $full
    kind = (Get-EnvKind $full)
    source = $source
    version = (Get-EnvVersion $full)
    active = (($env:VIRTUAL_ENV -and $full -ieq [IO.Path]::GetFullPath($env:VIRTUAL_ENV).TrimEnd('\')) -or
      ($env:CONDA_PREFIX -and $full -ieq [IO.Path]::GetFullPath($env:CONDA_PREFIX).TrimEnd('\')))
    bytes = (Get-EnvSize $full)
  }
}

function Find-Environments([string]$root, [string]$source) {
  if (-not $root -or -not (Test-Path -LiteralPath $root -PathType Container)) { return }
  $dirs = @()
  if ($PSVersionTable.PSVersion.Major -ge 7) {
    $dirs = @(Get-ChildItem -LiteralPath $root -Directory -Recurse -Depth $maxDepth -ErrorAction SilentlyContinue)
  } else {
    $dirs = @(Get-ChildItem -LiteralPath $root -Directory -Recurse -ErrorAction SilentlyContinue)
  }
  foreach ($d in $dirs) {
    if ($d.Name -match '^\.?venv$|^env$|^\.conda$|^anaconda|^miniconda|^python|^pyenv') {
      Add-Environment $d.FullName $source
    } elseif (Test-Path -LiteralPath (Join-Path $d.FullName 'pyvenv.cfg')) {
      Add-Environment $d.FullName $source
    } elseif (Test-Path -LiteralPath (Join-Path $d.FullName 'conda-meta\history')) {
      Add-Environment $d.FullName $source
    }
  }
}

$commandNames = @('python','python3','py','conda','mamba','micromamba','uv','poetry','pipx','virtualenv','pip','pip3','pymol')
$commands = @()
foreach ($name in $commandNames) {
  foreach ($item in @(Get-Command -Name $name -All -ErrorAction Ignore | Select-Object -First 8)) {
    $path = if ($item.Source) { [string]$item.Source } elseif ($item.Path) { [string]$item.Path } else { '' }
    if (-not $path) { continue }
    $commands += [pscustomobject]@{
      name = $name
      path = $path
      type = [string]$item.CommandType
      version = (Get-ToolVersion $path)
    }
  }
}

$pathEntries = @($env:PATH -split ';' | Where-Object { $_ -and $_ -match 'python|conda|mamba|uv|poetry|pipx|pyenv' })
$environmentVariables = [ordered]@{
  VIRTUAL_ENV = $env:VIRTUAL_ENV
  CONDA_PREFIX = $env:CONDA_PREFIX
  CONDA_DEFAULT_ENV = $env:CONDA_DEFAULT_ENV
  CONDA_SHLVL = $env:CONDA_SHLVL
  PYTHONHOME = $env:PYTHONHOME
  PYTHONPATH = $env:PYTHONPATH
  PIPX_HOME = $env:PIPX_HOME
  POETRY_HOME = $env:POETRY_HOME
  UV_PYTHON = $env:UV_PYTHON
  PATH_MATCHES = $pathEntries
}

$condaInfo = $null
$conda = $commands | Where-Object { $_.name -eq 'conda' } | Select-Object -First 1
if ($conda) {
  $text = (& $conda.path info --json 2>$null | Out-String)
  if ($text.Trim()) { try { $condaInfo = $text | ConvertFrom-Json -AsHashtable } catch { $condaInfo = $null } }
}

$pyLauncher = @()
$py = $commands | Where-Object { $_.name -eq 'py' } | Select-Object -First 1
if ($py) {
  $pyLauncher = @(& $py.path -0p 2>$null | ForEach-Object { ([string]$_).Trim() } | Where-Object { $_ })
}

$pipxJson = $null
$pipx = $commands | Where-Object { $_.name -eq 'pipx' } | Select-Object -First 1
if ($pipx) {
  $text = (& $pipx.path list --json 2>$null | Out-String)
  if ($text.Trim()) { try { $pipxJson = $text | ConvertFrom-Json -AsHashtable } catch { $pipxJson = $null } }
}

$poetryText = @()
$poetry = $commands | Where-Object { $_.name -eq 'poetry' } | Select-Object -First 1
if ($poetry) {
  $poetryText = @(& $poetry.path env list --full-path 2>$null | ForEach-Object { ([string]$_).Trim() } | Where-Object { $_ })
}

$pyenvText = @()
$pyenv = $commands | Where-Object { $_.name -eq 'pyenv' } | Select-Object -First 1
if ($pyenv) {
  $pyenvText = @(& $pyenv.path versions --bare 2>$null | ForEach-Object { ([string]$_).Trim() } | Where-Object { $_ })
}

$uvText = @()
$uv = $commands | Where-Object { $_.name -eq 'uv' } | Select-Object -First 1
if ($uv) {
  $uvText = @(& $uv.path python list --only-installed 2>$null | ForEach-Object { ([string]$_).Trim() } | Where-Object { $_ })
}

$seen = @{}
$envs = @()
if ($scanPath) { Find-Environments $scanPath 'scan-path' }
if ($env:VIRTUAL_ENV) { Add-Environment $env:VIRTUAL_ENV 'VIRTUAL_ENV' }
if ($env:CONDA_PREFIX) { Add-Environment $env:CONDA_PREFIX 'CONDA_PREFIX' }
if ($condaInfo) {
  foreach ($p in @($condaInfo.root_prefix) + @($condaInfo.envs) + @($condaInfo.envs_dirs)) {
    Add-Environment ([string]$p) 'conda'
  }
}
foreach ($p in @(
  (Join-Path $env:USERPROFILE '.conda\envs'),
  (Join-Path $env:USERPROFILE '.virtualenvs'),
  (Join-Path $env:USERPROFILE '.pyenv\versions'),
  (Join-Path $env:LOCALAPPDATA 'pypoetry\Cache\virtualenvs'),
  (Join-Path $env:LOCALAPPDATA 'uv\python'),
  (Join-Path $env:LOCALAPPDATA 'Programs\Python'),
  (Join-Path $env:ProgramFiles 'Python'),
  (Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Python'),
  (Join-Path $env:ProgramData 'Anaconda3'),
  (Join-Path $env:ProgramData 'miniconda3'),
  (Join-Path $env:USERPROFILE 'Anaconda3'),
  (Join-Path $env:USERPROFILE 'miniconda3')
)) {
  Find-Environments ([string]$p) 'known-root'
}
foreach ($p in $poetryText) {
  if ($p -notmatch '^\s*(No|Virtualenv|Path|In-project)' ) { Add-Environment $p 'poetry' }
}

ConvertTo-Json @{
  commands = $commands
  environmentVariables = $environmentVariables
  conda = $condaInfo
  pyLauncher = $pyLauncher
  pipx = $pipxJson
  poetry = $poetryText
  pyenv = $pyenvText
  uv = $uvText
  environments = $envs
} -Depth 10 -Compress
`;

export async function collectPython(
	params: PythonAuditInput,
	cwd: string,
	signal?: AbortSignal,
): Promise<Record<string, unknown>> {
	signal?.throwIfAborted();
	const scanPath = params.path ? params.path : cwd;
	const command = PYTHON_COMMAND.replace("__SCAN_PATH__", psString(scanPath))
		.replace("__MAX_DEPTH__", String(Math.max(1, Math.min(5, Math.floor(params.maxDepth)))))
		.replace("__INCLUDE_SIZES__", params.includeSizes ? "$true" : "$false");
	const raw = asRecord(await runPwsh(command, { signal }));
	const commands = asRecords(raw.commands ?? []);
	const environments = asRecords(raw.environments ?? []);
	const issues = derivePythonIssues(commands, environments, raw);
	const cleanupReview = environments
		.filter((env) => env.active !== true)
		.map((env) => ({
			path: env.path,
			kind: env.kind,
			manager: managerFromEnv(env),
			bytes: env.bytes ?? null,
			reason: "未被当前 VIRTUAL_ENV/CONDA_PREFIX 选中；删除前确认项目、解释器和包依赖仍在使用。",
		}))
		.sort((a, b) => Number(b.bytes ?? -1) - Number(a.bytes ?? -1));

	const common = {
		scope: params.scope,
		scanPath,
		issues,
		notice:
			"只读盘点：commands 来自 Get-Command；environments 来自当前 PATH、CONDA_PREFIX/VIRTUAL_ENV、conda/poetry 配置和已知目录。cleanupReview 只是待核查清单，不会执行删除；清理必须先由用户确认，再用 runbook 逐项生成命令。",
		...(raw.degraded ? { degraded: true, collectionErrors: raw.collectionErrors } : {}),
	};
	const sections: Record<string, unknown> = {};
	if (params.scope === "overview" || params.scope === "all") {
		sections.commands = commands;
		sections.environmentVariables = asRecord(raw.environmentVariables ?? {});
		sections.installations = deriveInstallations(commands, raw);
	}
	if (params.scope === "installations" || params.scope === "all") {
		sections.installations = deriveInstallations(commands, raw);
	}
	if (params.scope === "environments" || params.scope === "all") {
		sections.environments = environments;
		sections.cleanupReview = cleanupReview;
	}
	if (params.scope === "config" || params.scope === "all") {
		sections.environmentVariables = asRecord(raw.environmentVariables ?? {});
		sections.conda = raw.conda ?? null;
		sections.pyLauncher = raw.pyLauncher ?? [];
		sections.pipx = raw.pipx ?? null;
		sections.poetry = raw.poetry ?? [];
		sections.pyenv = raw.pyenv ?? [];
		sections.uv = raw.uv ?? [];
	}
	return { python: { ...common, ...sections } };
}

function normalizePath(value: unknown): string {
	return String(value ?? "")
		.replace(/[\\/]+$/, "")
		.toLowerCase();
}

function managerFromEnv(env: Record<string, unknown>): string {
	const kind = String(env.kind ?? "");
	const path = String(env.path ?? "").toLowerCase();
	if (kind === "conda" || path.includes("conda")) return path.includes("anaconda") ? "anaconda" : "conda";
	if (kind === "poetry" || path.includes("pypoetry")) return "poetry";
	if (kind === "uv" || path.includes("\\uv\\")) return "uv";
	if (kind === "pyenv" || path.includes("\\.pyenv\\")) return "pyenv";
	return "venv";
}

function deriveInstallations(
	commands: Record<string, unknown>[],
	raw: Record<string, unknown>,
): Record<string, unknown>[] {
	const seen = new Set<string>();
	const installations: Record<string, unknown>[] = [];
	for (const command of commands) {
		const name = String(command.name ?? "");
		const path = String(command.path ?? "");
		if (!path || !["python", "python3", "py", "conda", "mamba", "micromamba"].includes(name)) continue;
		const key = `${name}:${normalizePath(path)}`;
		if (seen.has(key)) continue;
		seen.add(key);
		installations.push({
			name,
			path,
			version: command.version ?? null,
			manager: name === "conda" || name === "mamba" || name === "micromamba" ? "conda" : "python",
		});
	}
	const conda = asRecord(raw.conda ?? {});
	for (const key of ["root_prefix", "active_prefix"]) {
		const path = String(conda[key] ?? "");
		if (!path || seen.has(`conda-root:${normalizePath(path)}`)) continue;
		seen.add(`conda-root:${normalizePath(path)}`);
		installations.push({ name: "conda-root", path, version: conda.conda_version ?? null, manager: "conda" });
	}
	return installations;
}

export function derivePythonIssues(
	commands: Record<string, unknown>[],
	environments: Record<string, unknown>[],
	raw: Record<string, unknown>,
): Record<string, unknown>[] {
	const issues: Record<string, unknown>[] = [];
	const pythonCommands = commands.filter((item) => ["python", "python3", "py"].includes(String(item.name)));
	const pythonPaths = new Set(pythonCommands.map((item) => normalizePath(item.path)));
	if (pythonPaths.size > 1) {
		issues.push({
			code: "multiple-python-commands",
			severity: "warning",
			message: `PATH/启动器解析到 ${pythonPaths.size} 个不同 Python 可执行文件，容易导致 pip 安装到错误环境。`,
		});
	}
	const managers = new Set(commands.map((item) => String(item.name)));
	const managerCount = ["conda", "mamba", "micromamba", "uv", "poetry", "pipx", "virtualenv"].filter((name) =>
		managers.has(name),
	).length;
	if (managerCount > 1) {
		issues.push({
			code: "multiple-package-managers",
			severity: "info",
			message: `检测到 ${managerCount} 类 Python 环境/包管理入口，先明确每个工具的职责，再合并或淘汰重复入口。`,
		});
	}
	const installations = deriveInstallations(commands, raw);
	const installPaths = installations.map((item) => String(item.path ?? "").toLowerCase()).join("\n");
	if (installPaths.includes("anaconda") && installPaths.includes("miniconda")) {
		issues.push({
			code: "anaconda-and-miniconda",
			severity: "warning",
			message: "同时存在 Anaconda 与 Miniconda。两者会重复占用空间并可能争抢 PATH；保留一个主根目录即可。",
		});
	}
	const conda = asRecord(raw.conda ?? {});
	const activePrefix = String(conda.active_prefix ?? "");
	const activeEnv = String(asRecord(raw.environmentVariables ?? {}).CONDA_PREFIX ?? "");
	const activePath = activeEnv || activePrefix;
	if (activePath && !environments.some((env) => normalizePath(env.path) === normalizePath(activePath))) {
		issues.push({
			code: "active-environment-not-found",
			severity: "warning",
			message: `当前激活环境 ${activePath} 未出现在环境清单中；可能是 PATH 残留、目录已删除或权限不可读。`,
		});
	}
	const pymol = commands.find((item) => String(item.name) === "pymol");
	if (pymol) {
		const pymolPath = String(pymol.path ?? "").toLowerCase();
		if (pymolPath.includes("conda")) {
			issues.push({
				code: "pymol-in-conda",
				severity: "info",
				message: "PyMOL 位于 Conda 环境内。若安装时落进 base/Anaconda 根环境，优先迁移到专用小环境。",
			});
		}
	}
	const v = asRecord(raw.environmentVariables ?? {});
	if (v.VIRTUAL_ENV && v.CONDA_PREFIX && normalizePath(v.VIRTUAL_ENV) !== normalizePath(v.CONDA_PREFIX)) {
		issues.push({
			code: "venv-and-conda-active",
			severity: "warning",
			message: "VIRTUAL_ENV 与 CONDA_PREFIX 同时存在且指向不同目录；当前 shell 的环境选择存在冲突。",
		});
	}
	return issues;
}
