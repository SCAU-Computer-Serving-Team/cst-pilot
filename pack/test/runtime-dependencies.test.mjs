import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { testRoot } from '../../agent/home/extensions/web/test/support.ts';
import { copyRuntimeDependencies } from '../runtime-dependencies.mjs';
import { checkReleaseTree } from '../release-checks.mjs';

const repository = fileURLToPath(new URL('../../', import.meta.url));

test('发行副本中的 Puppeteer 可独立加载，依赖闭包与许可证齐备，无浏览器或 Pi SDK 副本', async () => {
  const root = fs.mkdtempSync(path.join(await testRoot(), 'export-runtime-'));
  try {
    const packages = copyRuntimeDependencies(path.join(repository, 'agent'), path.join(root, 'agent'), ['puppeteer-core']);
    assert.equal(packages['node_modules/puppeteer-core'].version, '25.16.0');
    assert.ok(Object.keys(packages).length > 1);
    assert.equal(Object.keys(packages).some(name => name.includes('@earendil-works/')), false);
    const entry = path.join(root, 'agent/home/extensions/web/probe.mjs');
    fs.mkdirSync(path.dirname(entry), { recursive: true });
    fs.writeFileSync(entry, 'import puppeteer from "puppeteer-core"; console.log(typeof puppeteer.launch);');
    const result = spawnSync(process.execPath, [entry], { encoding: 'utf8', timeout: 15_000 });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), 'function');
    assert.ok(fs.statSync(path.join(repository, 'pack/licenses/puppeteer-LICENSE.txt')).size > 0);
    assert.equal(fs.existsSync(path.join(root, 'agent/node_modules/puppeteer')), false);
    const files = [];
    function collect(directory) {
      for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
        const file = path.join(directory, entry.name);
        if (entry.isDirectory()) collect(file);
        else files.push(path.relative(root, file).replaceAll('\\', '/'));
      }
    }
    collect(root);
    assert.doesNotThrow(() => checkReleaseTree(root, files));
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('运行依赖保留嵌套版本，跳过未安装可选项，缺失或版本漂移阻止装配', async () => {
  const root = fs.mkdtempSync(path.join(await testRoot(), 'runtime-graph-'));
  const source = path.join(root, 'source'), target = path.join(root, 'target');
  const packages = {};
  function write(relative, manifest) {
    fs.mkdirSync(path.join(source, relative), { recursive: true });
    fs.writeFileSync(path.join(source, relative, 'package.json'), JSON.stringify(manifest));
    packages[relative] = { version: manifest.version, integrity: 'sha512-fixture' };
  }
  try {
    write('node_modules/entry', { name: 'entry', version: '1.0.0', dependencies: { child: '1.0.0' }, optionalDependencies: { absent: '1.0.0' } });
    write('node_modules/entry/node_modules/child', { name: 'child', version: '1.0.0' });
    write('node_modules/child', { name: 'child', version: '2.0.0' });
    fs.mkdirSync(path.join(source, 'node_modules/entry/tests'));
    fs.writeFileSync(path.join(source, 'node_modules/entry/tests/fixture.test.js'), 'test-only');
    fs.writeFileSync(path.join(source, 'package-lock.json'), JSON.stringify({ packages }));
    const copied = copyRuntimeDependencies(source, target, ['entry']);
    assert.equal(copied['node_modules/entry/node_modules/child'].version, '1.0.0');
    assert.equal(copied['node_modules/child'], undefined);
    assert.equal(fs.existsSync(path.join(target, 'node_modules/entry/tests')), false);
    assert.equal(fs.existsSync(path.join(target, 'node_modules/entry/node_modules/child/package.json')), true);
    packages['node_modules/entry'].version = '2.0.0';
    fs.writeFileSync(path.join(source, 'package-lock.json'), JSON.stringify({ packages }));
    assert.throws(() => copyRuntimeDependencies(source, target, ['entry']), /锁文件不符/);
    assert.throws(() => copyRuntimeDependencies(source, target, ['missing']), /运行依赖缺失/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
