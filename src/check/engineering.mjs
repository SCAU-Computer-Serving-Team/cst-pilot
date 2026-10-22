import { execFileSync, spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const inventory = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard', '-z'], {
  cwd: root,
  encoding: 'utf8',
});
const files = [...new Set(inventory.split('\0').filter(file =>
  /\.(?:mjs|json|css)$/.test(file) &&
  !/(?:^|\/)(?:node_modules|build|static|\.react-router)\//.test(file)
))];
const biome = spawnSync(process.execPath, [
  join(root, 'agent/node_modules/@biomejs/biome/bin/biome'),
  'lint', '--error-on-warnings', '--max-diagnostics=100', ...files,
], { cwd: root, stdio: 'inherit' });
if (biome.error) throw biome.error;
if (biome.status !== 0) process.exit(biome.status ?? 1);

const powershell = spawnSync(process.platform === 'win32' ? 'pwsh.exe' : 'pwsh', [
  '-NoProfile', '-NonInteractive', '-File', join(root, 'src/check/powershell.ps1'),
], { cwd: root, stdio: 'inherit' });
if (powershell.error) throw powershell.error;
process.exitCode = powershell.status ?? 1;
