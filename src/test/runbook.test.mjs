import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { DefaultResourceLoader } from '../../agent/node_modules/@earendil-works/pi-coding-agent/dist/index.js';
import { testRoot } from '../../agent/home/extensions/web/test/support.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const parameters = { title: 'original', level: 'safe', items: [{ shell: 'cmd', summary: '显示测试文字，无系统改动', command: 'echo fixture' }] };

async function harness(t) {
  const kit = await mkdtemp(join(await testRoot(), 'runbook-engineering-'));
  const home = join(kit, 'agent/home');
  await cp(join(root, 'agent/home/extensions/diagnostics'), join(home, 'extensions/diagnostics'), { recursive: true });
  await writeFile(join(home, 'settings.json'), JSON.stringify({ packages: [], defaultProjectTrust: 'never' }));
  const loader = new DefaultResourceLoader({ cwd: kit, agentDir: home, noContextFiles: true, noSkills: true, noThemes: true });
  await loader.reload();
  assert.deepEqual(loader.getExtensions().errors, []);
  const tool = loader.getExtensions().extensions.flatMap(extension => [...extension.tools.values()]).find(tool => tool.definition.name === 'runbook').definition;
  t.after(() => rm(kit, { recursive: true, force: true }));
  const run = async extra => JSON.parse((await tool.execute('fixture', { ...parameters, ...extra }, new AbortController().signal, () => {}, { cwd: kit })).content[0].text).runbook;
  return { run, kit };
}

test('修正失败保留原清单，不先删除已交付内容', async t => {
  const { run } = await harness(t);
  const original = await run();
  const before = await readFile(original.file, 'utf8');
  // 目标文件名被目录占用，模拟新文件无法发布；原清单必须保持可用。
  await mkdir(join(original.dir, '01-安全-zz-blocked.txt'));
  await assert.rejects(run({ sequence: original.sequence, title: 'zz-blocked' }), /命令清单写入失败/);
  assert.equal(await readFile(original.file, 'utf8'), before);
});

test('修正只接受已存在的序号', async t => {
  const { run } = await harness(t);
  const original = await run();
  await assert.rejects(run({ sequence: 77 }), /序号.*不存在/);
  assert.deepEqual(await readdir(original.dir), [original.name]);
});

test('自动序号达到 999 后明确拒绝，不生成无法识别的编号', async t => {
  const { run } = await harness(t);
  const original = await run();
  await writeFile(join(original.dir, '999-安全-existing.txt'), 'existing');
  await assert.rejects(run({ title: 'overflow' }), /序号.*999/);
  assert.equal((await readdir(original.dir)).some(name => name.startsWith('1000-')), false);
});

test('同一清单目录被独立进程占用时拒绝写入，释放后恢复', async t => {
  const { run, kit } = await harness(t);
  const original = await run();
  const worker = join(kit, 'hold-directory-lock.mjs');
  await writeFile(worker, `
import { createHash } from 'node:crypto';
import { realpath } from 'node:fs/promises';
import { createServer } from 'node:net';
const path = await realpath(process.argv[2]);
const key = createHash('sha256').update(process.platform === 'win32' ? path.toLowerCase() : path).digest();
const address = process.platform === 'win32' ? '\\\\\\\\.\\\\pipe\\\\cst-pilot-runbook-' + key.toString('hex') : undefined;
const server = createServer(socket => socket.destroy());
const ready = () => console.log('ready');
if (address) server.listen(address, ready);
else server.listen({ host: '127.0.0.1', port: 32000 + key.readUInt32BE(0) % 20000, exclusive: true }, ready);
`);
  const child = spawn(process.execPath, [worker, original.dir], { stdio: ['ignore', 'pipe', 'pipe'] });
  const exited = once(child, 'exit');
  try {
    const ready = once(child.stdout, 'data');
    await Promise.race([
      ready,
      exited.then(() => { throw new Error('锁替身未启动'); }),
    ]);
    await assert.rejects(run({ title: 'occupied' }), /另一进程写入/);
    assert.deepEqual(await readdir(original.dir), [original.name]);
  } finally {
    child.kill();
    await exited;
  }
  const next = await run({ title: 'released' });
  assert.equal(next.sequence, 2);
});

test('同日并发交付序号唯一，修正改名后只保留新清单', async t => {
  const { run } = await harness(t);
  const results = await Promise.all(Array.from({ length: 4 }, (_, i) => run({ title: `item-${i}` })));
  assert.deepEqual(results.map(result => result.sequence), [1, 2, 3, 4]);
  const changed = await run({ sequence: 2, title: 'renamed' });
  assert.equal(changed.sequence, 2);
  const names = await readdir(changed.dir);
  assert.equal(names.length, 4);
  assert.ok(names.includes('02-安全-renamed.txt'));
  assert.ok(!names.includes(results[1].name));
});
