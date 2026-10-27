import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { requireSmokeAdministrator, smokeRelease } from '../smoke.mjs';

const repo = fileURLToPath(new URL('../../', import.meta.url));
const launcher = path.join(repo, 'launcher.ps1');
const pwsh = [process.env.CST_PILOT_TEST_PWSH, path.join(repo, 'pwsh/pwsh.exe'), 'C:/Program Files/PowerShell/7/pwsh.exe'].find(file => file && fs.existsSync(file));
const quote = value => `'${value.replaceAll("'", "''")}'`;
const encode = value => Buffer.from(value, 'utf16le').toString('base64');
const powershell = (code, options = {}) => {
  const result = spawnSync(pwsh, ['-NoProfile', '-NonInteractive', '-EncodedCommand', encode(`$ErrorActionPreference='Stop'; ${code}`)], { encoding: 'utf8', timeout: 30_000, windowsHide: true, ...options });
  assert.equal(result.error, undefined);
  return result;
};
function fixture() {
  const base = process.env.CST_WEB_TEST_TMPDIR || 'E:/tmp';
  const root = path.join(base, new Date().toISOString().slice(0, 10));
  fs.mkdirSync(root, { recursive: true });
  return fs.mkdtempSync(path.join(root, 'launcher-'));
}
const windows = { skip: process.platform !== 'win32' || !pwsh };

test('pi.cmd 保持 ASCII，避免 cmd 解码差异', () => {
  const entry = fs.readFileSync(path.join(repo, 'pi.cmd'));
  assert.ok(entry.every(byte => byte < 128));
});

test('未提权时只请求 RunAs，不初始化状态、不启动 Pi', windows, () => {
  const root = fixture();
  try {
    const result = powershell(`
      . ${quote(launcher)}
      function Test-PilotAdministrator { $false }
      function Test-PilotInteractive { $true }
      function Get-PilotWindowsTerminal { $null }
      function Initialize-PilotEnvironment { throw 'unexpected initialization' }
      function Invoke-PilotRuntime { throw 'unexpected runtime' }
      function Start-PilotWindowProcess { param($FilePath,$ArgumentList,$WorkingDirectory,$Verb,$ErrorAction)
        [Console]::WriteLine(([pscustomobject]@{file=$FilePath; arguments=$ArgumentList; cwd=$WorkingDirectory; verb=$Verb} | ConvertTo-Json -Compress))
      }
      Invoke-PilotLauncher -Root ${quote(root)} -Arguments @('--model','model with space')
      exit $script:PilotExitCode
    `);
    assert.equal(result.status, 0, result.stderr);
    const request = JSON.parse(result.stdout);
    assert.equal(request.verb, 'RunAs');
    assert.equal(request.file, path.join(root, 'pwsh/pwsh.exe'));
    assert.equal(request.arguments.at(-2), '-EncodedCommand');
    assert.equal(fs.existsSync(path.join(root, '.state')), false);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('取消 UAC 和请求失败均停止启动，不降级或重试', windows, () => {
  for (const [error, expected] of [[1223, 1223], [5, 1]]) {
    const result = powershell(`
      . ${quote(launcher)}
      function Test-PilotAdministrator { $false }
      function Test-PilotInteractive { $true }
      function Get-PilotWindowsTerminal { $null }
      function Initialize-PilotEnvironment { throw 'unexpected initialization' }
      function Invoke-PilotRuntime { throw 'unexpected runtime' }
      function Start-PilotWindowProcess { throw [ComponentModel.Win32Exception]::new(${error}) }
      Invoke-PilotLauncher -Root ${quote(repo)} -Arguments @()
      exit $script:PilotExitCode
    `);
    assert.equal(result.status, expected, result.stderr);
    assert.match(result.stderr, error === 1223 ? /cancelled/ : /cannot request administrator/);
    assert.doesNotMatch(result.stderr, /unexpected/);
  }
});

test('提权载荷原样保存参数与原 cwd，特殊字符不进入可执行代码', windows, () => {
  const root = fixture();
  try {
    const args = ['', '--model', '中文 space', "quote'\" & | < > ^ ! %PATH% ` $env:USERPROFILE", 'C:\\trailing\\'];
    const request = Buffer.from(JSON.stringify(args), 'utf8').toString('base64');
    const result = powershell(`
      . ${quote(launcher)}
      $forwarded=[string[]](ConvertFrom-Json ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${request}'))))
      New-PilotElevationCommand -Root ${quote(repo)} -WorkingDirectory ${quote(root)} -Arguments $forwarded
    `);
    assert.equal(result.status, 0, result.stderr);
    const command = Buffer.from(result.stdout.trim(), 'base64').toString('utf16le');
    const mock = `
      function Test-PilotAdministrator { $true }
      function Invoke-PilotLauncher { param($Root,[string[]]$Arguments)
        [pscustomobject]@{root=$Root; args=$Arguments; cwd=(Get-Location).Path} | ConvertTo-Json -Compress
        $script:PilotExitCode=0
      }
    `;
    const roundtrip = powershell(command.replace('. $request.launcher', `. $request.launcher\n${mock}`));
    assert.equal(roundtrip.status, 0, roundtrip.stderr);
    const value = JSON.parse(roundtrip.stdout);
    assert.deepEqual(value.args, args);
    assert.equal(value.cwd, root);
    assert.equal(value.root, repo.replace(/[\\/]$/, ''));
    const denied = powershell(command.replace('. $request.launcher', '. $request.launcher\nfunction Test-PilotAdministrator { $false }'));
    assert.equal(denied.status, 1);
    assert.match(denied.stderr, /administrator permissions were not granted/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('提权载荷保留卷根目录，盘符不变为相对路径', windows, () => {
  const volume = path.parse(repo).root;
  const result = powershell(`
    . ${quote(launcher)}
    New-PilotElevationCommand -Root ${quote(volume)} -WorkingDirectory ${quote(repo)} -Arguments @()
  `);
  assert.equal(result.status, 0, result.stderr);
  const command = Buffer.from(result.stdout.trim(), 'base64').toString('utf16le');
  const payload = /FromBase64String\('([^']+)'\)/.exec(command);
  assert.ok(payload);
  const request = JSON.parse(Buffer.from(payload[1], 'base64').toString('utf8'));
  assert.equal(request.root, volume);
  assert.equal(request.launcher, path.join(volume, 'launcher.ps1'));
});

test('已提权启动保持环境隔离、原 cwd、原参数与退出码，.bin 可直接执行', windows, () => {
  const root = fixture();
  try {
    fs.writeFileSync(path.join(root, 'pi.cmd'), '@echo off\r\n');
    const runtime = path.join(root, 'probe.bin');
    fs.copyFileSync(process.execPath, runtime);
    const probe = path.join(root, 'probe.cjs');
    fs.writeFileSync(probe, "console.log(JSON.stringify({args:process.argv.slice(2),cwd:process.cwd(),home:process.env.PI_CODING_AGENT_DIR,tmp:process.env.TEMP,path:process.env.PATH,key:process.env.OPENAI_API_KEY,override:process.env.CST_PILOT_PWSH,offline:process.env.PI_OFFLINE}));process.exit(23);");
    const result = powershell(`
      . ${quote(launcher)}
      function Test-PilotAdministrator { $true }
      function Start-PilotWindowProcess { throw 'unexpected UAC' }
      function Get-PilotRuntime { param($Root)
        @{file=${quote(runtime)}; arguments=@(${quote(probe)})}
      }
      $env:OPENAI_API_KEY='test-only'; $env:CST_PILOT_PWSH='untrusted.exe'; $env:PI_INHERIT_HOST_PATH=$null
      Set-Location -LiteralPath ${quote(root)}
      Invoke-PilotLauncher -Root ${quote(root)} -Arguments @('--model','中文 space','', 'a"b', 'a&b', ${quote('C:\\trailing\\')})
      exit $script:PilotExitCode
    `);
    assert.equal(result.status, 23, result.stderr);
    const value = JSON.parse(result.stdout);
    assert.deepEqual(value.args, ['--model', '中文 space', '', 'a"b', 'a&b', 'C:\\trailing\\']);
    assert.equal(value.cwd, root);
    assert.equal(value.home, path.join(root, 'agent/home'));
    assert.equal(value.tmp, path.join(root, '.state/tmp'));
    assert.equal(value.offline, '1');
    assert.equal(value.key, undefined);
    assert.equal(value.override, undefined);
    assert.ok(value.path.split(';').includes(path.join(root, 'pwsh')));
    assert.ok(fs.statSync(path.join(root, '.state/cache')).isDirectory());
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('发行二进制与开发运行时保留原启动参数', windows, () => {
  const root = fixture();
  try {
    fs.mkdirSync(path.join(root, 'agent/.runtime'), { recursive: true });
    const readRuntime = () => {
      const result = powershell(`. ${quote(launcher)}; Get-PilotRuntime -Root ${quote(root)} | ConvertTo-Json -Compress`);
      assert.equal(result.status, 0, result.stderr);
      return JSON.parse(result.stdout);
    };
    const development = readRuntime();
    assert.equal(development.file, path.join(root, 'node/node.exe'));
    assert.match(development.arguments[0], /cli\.js$/);
    fs.writeFileSync(path.join(root, 'agent/.runtime/prx.bin'), 'fixture');
    const release = readRuntime();
    assert.equal(release.file, path.join(root, 'agent/.runtime/prx.bin'));
    assert.deepEqual(release.arguments, ['--no-skills', '--skill', path.join(root, 'agent/home/skills'), '--no-context-files']);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('状态目录不可写时不启动 Pi', windows, () => {
  const root = fixture();
  try {
    fs.writeFileSync(path.join(root, '.state'), 'not a directory');
    const result = powershell(`
      . ${quote(launcher)}
      function Test-PilotAdministrator { $true }
      function Invoke-PilotRuntime { throw 'unexpected runtime' }
      Invoke-PilotLauncher -Root ${quote(root)} -Arguments @()
      exit $script:PilotExitCode
    `);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /cannot create or write state directories/);
    assert.doesNotMatch(result.stderr, /unexpected runtime/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('Pi 子进程继承标准输入输出，RPC 响应与非零退出码可透传', windows, () => {
  const root = fixture();
  try {
    const probe = path.join(root, 'rpc.cjs');
    fs.writeFileSync(probe, "process.stdin.setEncoding('utf8');let input='';process.stdin.on('data',s=>input+=s);process.stdin.on('end',()=>{console.log('RPC:'+input.trim());console.error('RPC_STDERR');process.exit(19)});");
    const result = powershell(`
      . ${quote(launcher)}
      function Get-PilotRuntime { param($Root) @{file=${quote(process.execPath)};arguments=@(${quote(probe)})} }
      Invoke-PilotRuntime -Root ${quote(root)} -Arguments @()
      exit $script:PilotExitCode
    `, { input: 'RPC_REQUEST\n' });
    assert.equal(result.status, 19, result.stderr);
    assert.equal(result.stdout.trim(), 'RPC:RPC_REQUEST');
    assert.match(result.stderr, /RPC_STDERR/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('真实 cmd 入口支持特殊路径、中文参数、空参数和尾部反斜线', windows, () => {
  const base = fixture();
  const root = path.join(base, "kit 中文 ' & % ! [literal]");
  try {
    fs.mkdirSync(root);
    fs.copyFileSync(path.join(repo, 'pi.cmd'), path.join(root, 'pi.cmd'));
    fs.symlinkSync(path.dirname(pwsh), path.join(root, 'pwsh'), 'junction');
    const probe = path.join(root, 'argv.cjs');
    fs.writeFileSync(probe, "console.log(JSON.stringify({args:process.argv.slice(2),cwd:process.cwd()}));process.exit(17);");
    const mock = `
function Test-PilotAdministrator { $true }
function Start-PilotWindowProcess { throw 'unexpected UAC' }
function Get-PilotRuntime { param($Root) @{file=${quote(process.execPath)};arguments=@(${quote(probe)})} }
`;
    const source = fs.readFileSync(launcher, 'utf8').replace("if ($MyInvocation.InvocationName -ne '.') {", `${mock}\nif ($MyInvocation.InvocationName -ne '.') {`);
    fs.writeFileSync(path.join(root, 'launcher.ps1'), source);
    const args = ['--model', '中文/model space', '--print', "safe & | < > ^ ! % text ' words", '', 'C:\\trailing\\'];
    const windowsQuote = value => `"${value.replace(/(\\*)"/g, '$1$1\\"').replace(/(\\+)$/g, '$1$1')}"`;
    const command = `""${path.join(root, 'pi.cmd')}" ${args.map(windowsQuote).join(' ')}"`;
    const result = spawnSync(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', command], {
      cwd: base, encoding: 'utf8', timeout: 30_000, windowsHide: true, windowsVerbatimArguments: true,
    });
    assert.equal(result.status, 17, result.stderr);
    const value = JSON.parse(result.stdout.trim());
    assert.deepEqual(value.args, args);
    assert.equal(value.cwd, base);
    assert.ok(fs.statSync(path.join(root, '.state/tmp')).isDirectory());
  } finally { fs.rmSync(base, { recursive: true, force: true }); }
});

test('开发运行时实际启动 Pi，版本参数与退出码正确', {
  skip: windows.skip || !fs.existsSync(path.join(repo, 'agent/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js')),
}, () => {
  const root = fixture();
  try {
    fs.copyFileSync(path.join(repo, 'pi.cmd'), path.join(root, 'pi.cmd'));
    fs.mkdirSync(path.join(root, 'agent/home/skills'), { recursive: true });
    fs.symlinkSync(path.join(repo, 'agent/node_modules'), path.join(root, 'agent/node_modules'), 'junction');
    fs.symlinkSync(path.dirname(process.execPath), path.join(root, 'node'), 'junction');
    const result = powershell(`
      . ${quote(launcher)}
      function Test-PilotAdministrator { $true }
      function Start-PilotWindowProcess { throw 'unexpected UAC' }
      Set-Location -LiteralPath ${quote(root)}
      Invoke-PilotLauncher -Root ${quote(root)} -Arguments @('--version')
      exit $script:PilotExitCode
    `);
    assert.equal(result.status, 0, result.stderr);
    const version = JSON.parse(fs.readFileSync(path.join(repo, 'agent/package.json'), 'utf8')).dependencies['@earendil-works/pi-coding-agent'];
    assert.equal(result.stdout.trim(), version);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('未提权的管道调用直接失败，不弹 UAC 或返回成功', windows, () => {
  const result = powershell(`
    . ${quote(launcher)}
    function Test-PilotAdministrator { $false }
    function Test-PilotInteractive { $false }
    function Start-PilotWindowProcess { throw 'unexpected UAC' }
    function Initialize-PilotEnvironment { throw 'unexpected initialization' }
    function Invoke-PilotRuntime { throw 'unexpected runtime' }
    Invoke-PilotLauncher -Root ${quote(repo)} -Arguments @('--mode','rpc')
    exit $script:PilotExitCode
  `);
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /piped execution requires an administrator terminal/);
  assert.doesNotMatch(result.stderr, /unexpected/);
});

test('普通权限交互启动优先在新的 Windows Terminal 中申请管理员授权', windows, () => {
  const result = powershell(`
    . ${quote(launcher)}
    function Test-PilotAdministrator { $false }
    function Test-PilotInteractive { $true }
    function Get-PilotWindowsTerminal { 'C:\\WindowsApps\\wt.exe' }
    function Initialize-PilotEnvironment { throw 'unexpected initialization' }
    function Invoke-PilotRuntime { throw 'unexpected runtime' }
    function Start-PilotWindowProcess { param($FilePath,$ArgumentList,$WorkingDirectory,$Verb,$ErrorAction)
      [Console]::WriteLine((@{file=$FilePath; arguments=$ArgumentList; verb=$Verb} | ConvertTo-Json -Compress))
    }
    Invoke-PilotLauncher -Root ${quote(repo)} -Arguments @('--model','中文/model space')
    exit $script:PilotExitCode
  `);
  assert.equal(result.status, 0, result.stderr);
  const request = JSON.parse(result.stdout);
  assert.equal(request.file, 'C:\\WindowsApps\\wt.exe');
  assert.equal(request.verb, 'RunAs');
  assert.deepEqual(request.arguments.slice(0, 3), ['-w', 'new', 'new-tab']);
  assert.equal(request.arguments[request.arguments.indexOf('--title') + 1], 'CST Pilot');
  assert.equal(request.arguments.at(-2), '-EncodedCommand');
  assert.match(request.arguments.at(-1), /^[A-Za-z0-9+/=]+$/);
});

test('已提权的传统窗口转入 wt；管理员 wt 或已安排窗口不重复启动', windows, () => {
  for (const [session, ready, expected] of [['', false, 'wt'], ['existing-session', false, 'direct'], ['', true, 'direct']]) {
    const result = powershell(`
      . ${quote(launcher)}
      function Test-PilotAdministrator { $true }
      function Test-PilotInteractive { $true }
      function Get-PilotWindowsTerminal { 'C:\\WindowsApps\\wt.exe' }
      function Initialize-PilotEnvironment { }
      function Invoke-PilotRuntime { [Console]::WriteLine('direct'); $script:PilotExitCode=0 }
      function Start-PilotWindowProcess { [Console]::WriteLine('wt') }
      $env:WT_SESSION=${quote(session)}
      Invoke-PilotLauncher -Root ${quote(repo)} -Arguments @() -ConsoleReady:$${ready}
      exit $script:PilotExitCode
    `);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), expected);
  }
});

test('wt 启动失败回退 PowerShell；取消 UAC 时停止，不二次请求', windows, () => {
  for (const [error, expected, attempts] of [[5, 0, 2], [1223, 1223, 1]]) {
    const result = powershell(`
      . ${quote(launcher)}
      function Test-PilotAdministrator { $false }
      function Test-PilotInteractive { $true }
      function Get-PilotWindowsTerminal { 'C:\\WindowsApps\\wt.exe' }
      function Initialize-PilotEnvironment { throw 'unexpected initialization' }
      function Invoke-PilotRuntime { throw 'unexpected runtime' }
      $script:attempts=0
      function Start-PilotWindowProcess { param($FilePath)
        $script:attempts++
        if ($FilePath.EndsWith('wt.exe')) { throw [ComponentModel.Win32Exception]::new(${error}) }
      }
      Invoke-PilotLauncher -Root ${quote(repo)} -Arguments @()
      [Console]::WriteLine($script:attempts)
      exit $script:PilotExitCode
    `);
    assert.equal(result.status, expected, result.stderr);
    assert.equal(Number(result.stdout.trim()), attempts);
    assert.match(result.stderr, error === 1223 ? /cancelled/ : /Windows Terminal.*falling back/);
  }
});

test('未安装 wt 时保持管理员授权路径，重启载荷防止窗口循环', windows, () => {
  const result = powershell(`
    . ${quote(launcher)}
    function Test-PilotAdministrator { $false }
    function Test-PilotInteractive { $true }
    function Get-PilotWindowsTerminal { $null }
    function Start-PilotWindowProcess { param($FilePath,$ArgumentList)
      [Console]::WriteLine((@{file=$FilePath; arguments=$ArgumentList} | ConvertTo-Json -Compress))
    }
    Invoke-PilotLauncher -Root ${quote(repo)} -Arguments @()
    exit $script:PilotExitCode
  `);
  assert.equal(result.status, 0, result.stderr);
  const request = JSON.parse(result.stdout);
  assert.equal(request.file, path.join(repo, 'pwsh/pwsh.exe'));
  const command = Buffer.from(request.arguments.at(-1), 'base64').toString('utf16le');
  assert.match(command, /-ConsoleReady/);
});

test('Windows 窗口 API 原样传递特殊路径、空参数和引号', windows, () => {
  const root = fixture();
  try {
    const args = ['', '中文 space', "quote'\" & | < > ^ ! %PATH% ` $env:USERPROFILE", 'C:\\trailing\\'];
    const args64 = Buffer.from(JSON.stringify(args), 'utf8').toString('base64');
    const probe = path.join(root, 'argv.cjs');
    const report = path.join(root, 'argv.json');
    const directory = path.join(root, "cwd 中文 ' & [literal]");
    fs.mkdirSync(directory);
    fs.writeFileSync(probe, `require('fs').writeFileSync(${JSON.stringify(report)},JSON.stringify({args:process.argv.slice(2),cwd:process.cwd()}));`);
    const result = powershell(`
      . ${quote(launcher)}
      $values=[string[]](ConvertFrom-Json ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${args64}'))))
      Start-PilotWindowProcess -FilePath ${quote(process.execPath)} -ArgumentList (@(${quote(probe)}) + @($values)) -WorkingDirectory ${quote(directory)} -Verb Open
      $deadline=(Get-Date).AddSeconds(10)
      while (-not (Test-Path -LiteralPath ${quote(report)}) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 20 }
      if (-not (Test-Path -LiteralPath ${quote(report)})) { throw 'argument probe did not finish' }
    `);
    assert.equal(result.status, 0, result.stderr);
    const value = JSON.parse(fs.readFileSync(report, 'utf8'));
    assert.deepEqual(value.args, args);
    assert.equal(value.cwd, directory);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('发行冒烟在未提权时提前失败，不复制发行树或弹出 UAC', windows, async () => {
  const root = fixture();
  try {
    fs.copyFileSync(launcher, path.join(root, 'launcher.ps1'));
    fs.symlinkSync(path.dirname(pwsh), path.join(root, 'pwsh'), 'junction');
    const permission = powershell(`. ${quote(launcher)}; Test-PilotAdministrator | ConvertTo-Json -Compress`);
    assert.equal(permission.status, 0, permission.stderr);
    if (JSON.parse(permission.stdout)) {
      assert.doesNotThrow(() => requireSmokeAdministrator(root));
    } else {
      const work = path.join(root, 'smoke-work');
      await assert.rejects(smokeRelease(root, work), /发行冒烟需要在已取得管理员权限的终端运行/);
      assert.equal(fs.existsSync(work), false);
    }
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('缺少随包 PowerShell 时退出，不借用宿主运行时', windows, () => {
  const root = fixture();
  try {
    fs.copyFileSync(path.join(repo, 'pi.cmd'), path.join(root, 'pi.cmd'));
    const result = spawnSync(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', `""${path.join(root, 'pi.cmd')}""`], {
      cwd: root, encoding: 'utf8', windowsVerbatimArguments: true, windowsHide: true, timeout: 10_000,
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /bundled pwsh.*is missing/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
