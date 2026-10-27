import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

const workflow = readFileSync(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8');
const step = workflow.match(/name: 准备固定版本 PowerShell 静态检查\r?\n\s+shell: pwsh\r?\n\s+run: \|\r?\n((?: {10,}[^\n]*\r?\n)+)/)?.[1];

test('CI 在 PSGallery 缺失时注册，已有配置时保持不变，始终安装固定版本', () => {
  assert.ok(step);
  const root = mkdtempSync(join(process.env.CST_WEB_TEST_TMPDIR || tmpdir(), 'cst-ci-bootstrap-'));
  try {
    for (const registered of [false, true]) {
      const script = `
$ErrorActionPreference = 'Stop'
$script:registered = $${registered}
$script:registrations = 0
function Get-PSRepository { param($Name, $ErrorAction) if ($script:registered) { [pscustomobject]@{ Name = 'PSGallery' } } }
function Register-PSRepository { param([switch]$Default, $ErrorAction) if (!$Default) { throw 'Expected default repository' }; $script:registered = $true; $script:registrations++ }
function Save-Module { param($Name, $RequiredVersion, $Repository, $Path, [switch]$Force, $ErrorAction)
  if (!$script:registered) { throw 'PSGallery missing' }
  if ($Name -ne 'PSScriptAnalyzer' -or $RequiredVersion -ne '1.24.0' -or $Repository -ne 'PSGallery') { throw 'Unexpected module source/version' }
}
& {
${step}
}
if ($script:registrations -ne ${registered ? 0 : 1}) { throw 'Repository registration count mismatch' }
Write-Output 'bootstrap-ok'
`;
      const output = execFileSync(process.platform === 'win32' ? 'pwsh.exe' : 'pwsh', ['-NoProfile', '-NonInteractive', '-Command', script], {
        encoding: 'utf8',
        env: { ...process.env, RUNNER_TEMP: root, GITHUB_ENV: join(root, 'github-env') },
      });
      assert.match(output, /bootstrap-ok/);
      assert.match(readFileSync(join(root, 'github-env'), 'utf8'), /PSScriptAnalyzer\/1\.24\.0\/PSScriptAnalyzer\.psd1/);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
