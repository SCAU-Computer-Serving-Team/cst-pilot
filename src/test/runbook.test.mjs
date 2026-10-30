import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { createAgentSession, DefaultResourceLoader, SessionManager, SettingsManager } from '../../agent/node_modules/@earendil-works/pi-coding-agent/dist/index.js';
import { createAssistantMessageEventStream } from '../../agent/node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai/dist/index.js';
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
  const tools = loader.getExtensions().extensions.flatMap(extension => [...extension.tools.values()]).map(tool => tool.definition);
  const tool = tools.find(tool => tool.name === 'runbook');
  t.after(() => rm(kit, { recursive: true, force: true }));
  const run = async extra => JSON.parse((await tool.execute('fixture', { ...parameters, ...extra }, new AbortController().signal, () => {}, { cwd: kit })).content[0].text).runbook;
  const call = async (name, params) => {
    const definition = loader.getExtensions().extensions.flatMap(extension => [...extension.tools.values()]).map(tool => tool.definition).find(tool => tool.name === name);
    assert.ok(definition, `工具已注册：${name}`);
    const response = await definition.execute('fixture', params, new AbortController().signal, () => {}, { cwd: kit });
    return response.details.runbook;
  };
  return { run, kit, home, loader, call, reload: () => loader.reload() };
}

test('指定日期统一写入 artifacts 日期目录，非法日期拒绝且不越界', async t => {
  const { run, call, kit } = await harness(t);
  const single = await run({ date: '2026-10-29' });
  assert.equal(single.dir, join(kit, 'artifacts', '2026-10-29', 'runbook'));
  const plan = await call('command_plan', { ...parameters, date: '2026-10-28' });
  const delivered = await call('runbook_deliver', { runbookId: plan.runbookId, version: 1 });
  assert.equal(delivered.dir, join(kit, 'artifacts', '2026-10-28', 'runbook', plan.runbookId));
  await assert.rejects(run({ date: '2026-02-30' }), /日期/);
  await assert.rejects(call('command_plan', { ...parameters, date: '../outside' }), /日期/);
  await assert.rejects(call('command_plan', { ...parameters, runbookId: plan.runbookId, baseVersion: 1, date: '2026-10-27' }), /日期/);
});

test('已有方案保留，重新加载后可读取并交付到统一产物目录', async t => {
  const { call, kit, home, reload } = await harness(t);
  const plan = await call('command_plan', parameters);
  const saved = join(home, 'command-plans', plan.runbookId);
  const retained = join(kit, 'outbox', '.plans', plan.runbookId);
  await cp(saved, retained, { recursive: true });
  await rm(join(home, 'command-plans'), { recursive: true });
  await reload();
  const delivered = await call('runbook_deliver', { runbookId: plan.runbookId, version: 1 });
  assert.ok(delivered.file.startsWith(join(kit, 'artifacts', plan.day, 'runbook')));
  assert.ok(await readFile(join(retained, 'v1.json'), 'utf8'));
  const revised = await call('command_plan', { ...parameters, runbookId: plan.runbookId, baseVersion: 1 });
  assert.equal(revised.version, 2);
});

test('真实 Pi 连续两轮执行方案与交付，模型收到新工具及上一版完整结果', async t => {
  const h = await harness(t);
  await writeFile(join(h.home, 'models.json'), JSON.stringify({ providers: { fixture: { baseUrl: 'http://127.0.0.1/v1', api: 'openai-completions', apiKey: 'fixture-only', models: [{ id: 'fixture', name: 'fixture', reasoning: false, input: ['text'], contextWindow: 16000, maxTokens: 300 }] } } }));
  const model = { id: 'fixture', name: 'fixture', api: 'openai-completions', provider: 'fixture', baseUrl: 'http://127.0.0.1', reasoning: false, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 16000, maxTokens: 300 };
  const { session } = await createAgentSession({ cwd: h.kit, agentDir: h.home, resourceLoader: h.loader, model, sessionManager: SessionManager.create(h.kit, join(h.home, 'sessions')), settingsManager: SettingsManager.create(h.kit, h.home) });
  t.after(() => session.dispose());
  let requests = 0, current;
  const delivered = [];
  session.agent.streamFunction = (_model, context) => {
    assert.ok(context.tools.some(tool => tool.name === 'command_plan'));
    assert.ok(context.tools.some(tool => tool.name === 'runbook_deliver'));
    const stage = requests++ % 3;
    if (stage !== 0) {
      const response = context.messages.findLast(message => message.role === 'toolResult');
      assert.equal(response.isError, false);
      current = JSON.parse(response.content[0].text).runbook;
      assert.ok(current.commands.length);
      if (stage === 2) delivered.push(current);
    }
    const content = stage === 0 ? [{ type: 'toolCall', id: 'plan-call', name: 'command_plan', arguments: { ...parameters, ...(current ? { runbookId: current.runbookId, baseVersion: current.version } : {}), items: [{ ...parameters.items[0], command: requests < 3 ? 'echo first' : 'echo second' }] } }] : stage === 1 ? [{ type: 'toolCall', id: 'delivery-call', name: 'runbook_deliver', arguments: { runbookId: current.runbookId, version: current.version } }] : [{ type: 'text', text: '请逐条复制执行。' }];
    const message = { role: 'assistant', content, api: model.api, provider: model.provider, model: model.id, usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: stage === 2 ? 'stop' : 'toolUse', timestamp: Date.now() };
    const stream = createAssistantMessageEventStream();
    queueMicrotask(() => { stream.push({ type: 'done', reason: message.stopReason, message }); stream.end(); });
    return stream;
  };
  await session.prompt('准备并交付第一批命令，已核对。');
  await session.prompt('修改命令并交付第二版，已核对。');
  assert.equal(requests, 6);
  assert.deepEqual(delivered.map(plan => plan.version), [1, 2]);
  assert.equal(delivered[0].runbookId, delivered[1].runbookId);
  assert.match(await readFile(delivered[0].file, 'utf8'), /echo first/);
  assert.match(await readFile(delivered[1].file, 'utf8'), /echo second/);
});

test('方案可跨加载恢复，交付不需要重复命令，同版本重试不重复生成文件', async t => {
  const { call, kit, reload } = await harness(t);
  const plan = await call('command_plan', parameters);
  await reload();
  assert.equal(plan.version, 1);
  assert.equal(plan.commands[0].command, 'echo fixture');
  assert.equal(plan.file, undefined);
  const delivered = await call('runbook_deliver', { runbookId: plan.runbookId, version: plan.version });
  assert.deepEqual(delivered.commands, plan.commands);
  assert.equal(delivered.runbookId, plan.runbookId);
  assert.match(await readFile(delivered.file, 'utf8'), /echo fixture/);
  const again = await call('runbook_deliver', { runbookId: plan.runbookId, version: plan.version });
  assert.equal(again.file, delivered.file);
  assert.equal((await readdir(delivered.dir)).filter(name => name.endsWith('.txt')).length, 1);
  assert.ok(delivered.file.startsWith(join(kit, 'artifacts', plan.day, 'runbook')));
});

test('修订保持 ID 并递增版本，旧版本和并发覆盖被拒绝，导出内容互不改写', async t => {
  const { call } = await harness(t);
  const original = await call('command_plan', parameters);
  const first = await call('runbook_deliver', { runbookId: original.runbookId, version: 1 });
  const changed = { ...parameters, runbookId: original.runbookId, baseVersion: 1, items: [{shell:'powershell',command:'Write-Output revised',summary:'输出修订内容'}] };
  const outcomes = await Promise.allSettled([call('command_plan', changed), call('command_plan', changed)]);
  assert.equal(outcomes.filter(result => result.status === 'fulfilled').length, 1);
  const revised = outcomes.find(result => result.status === 'fulfilled').value;
  assert.equal(revised.runbookId, original.runbookId);
  assert.equal(revised.version, 2);
  await assert.rejects(call('runbook_deliver', {runbookId:original.runbookId,version:1}), /新版本 2/);
  const second = await call('runbook_deliver', {runbookId:original.runbookId,version:2});
  assert.notEqual(first.file, second.file);
  assert.match(await readFile(first.file, 'utf8'), /echo fixture/);
  assert.match(await readFile(second.file, 'utf8'), /Write-Output revised/);
  await writeFile(second.file, '现场已修改');
  await assert.rejects(call('runbook_deliver', {runbookId:original.runbookId,version:2}), /被修改/);
});

test('同版本清单换时区后重试保持文件内容，不误判现场修改', async t => {
  const { call } = await harness(t);
  const previous = process.env.TZ;
  try {
    process.env.TZ = 'UTC';
    const plan = await call('command_plan', parameters);
    const original = await call('runbook_deliver', {runbookId:plan.runbookId,version:1});
    const body = await readFile(original.file, 'utf8');
    process.env.TZ = 'Pacific/Honolulu';
    const repeated = await call('runbook_deliver', {runbookId:plan.runbookId,version:1});
    assert.equal(repeated.file, original.file);
    assert.equal(await readFile(repeated.file, 'utf8'), body);
  } finally {
    if (previous === undefined) delete process.env.TZ; else process.env.TZ = previous;
  }
});

test('方案校验拒绝换行、越界数量、缺失修订版本与路径注入', async t => {
  const { call } = await harness(t);
  await assert.rejects(call('command_plan',{...parameters,items:[{...parameters.items[0],command:'echo first\necho second'}]}), /换行/);
  await assert.rejects(call('command_plan',{...parameters,items:Array(21).fill(parameters.items[0])}), /1–20/);
  await assert.rejects(call('command_plan',{...parameters,runbookId:'../outside'}), /同时提供/);
  await assert.rejects(call('runbook_deliver',{runbookId:'../outside',version:1}), /ID 无效/);
});

test('交付结果包含可直接展示的标准化命令', async t => {
  const { run } = await harness(t);
  const result = await run();
  assert.deepEqual(result.commands, [
    {
      summary: '显示测试文字，无系统改动',
      command: 'echo fixture',
      shell: 'cmd',
      admin: false,
    },
  ]);
});

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
