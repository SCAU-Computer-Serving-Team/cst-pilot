import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { randomUUID } from 'node:crypto';
import childProcess, { spawn } from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';
import { test } from 'node:test';
import { createAgentSession, DefaultResourceLoader, SessionManager, SettingsManager } from '../../agent/node_modules/@earendil-works/pi-coding-agent/dist/index.js';
import { createAssistantMessageEventStream } from '../../agent/node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai/dist/index.js';
import { getThemeByName, resolveThemeSetting } from '../../agent/node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const source = join(root, 'agent/home/extensions/diagnostics');
const password = Array(8).fill('000000').join('-'); // Public fixture, never a machine credential.
const theme = { fg: (_name, text) => text };
function volume(drive = 'D:', extra = {}) {
  return { volumeId: `fixture-${drive}`, drive, volumeType: 1, conversionStatus: 1,
    encryptionPercentage: 100, encryptionMethod: 6, protectionStatus: 1, lockStatus: 1,
    autoUnlockEnabled: false, autoUnlockKeysStored: null, keyProtectors: [{ id: 'fixture-id', type: 3 }],
    collectionErrors: [], ...extra };
}
const snapshot = volumes => ({ collectedAt: '2026-10-13T00:00:00.000Z', admin: true, volumes: structuredClone(volumes), collectionErrors: [] });
const accepted = { status: 'accepted', reason: 'Windows 已接受请求', returnCode: 0, error: null };

async function harness(t) {
  const base = process.env.CST_WEB_TEST_TMPDIR || (process.platform === 'win32' ? (existsSync('E:/') ? 'E:/tmp' : 'F:/tmp') : tmpdir());
  const dated = join(base, new Date().toISOString().slice(0, 10));
  await mkdir(dated, { recursive: true });
  const dir = await mkdtemp(join(dated, 'bitlocker-test-')), home = join(dir, 'agent/home');
  const extension = join(home, 'extensions/diagnostics');
  await cp(source, extension, { recursive: true });
  await cp(join(root, 'agent/home/skills/bitlocker'), join(home, 'skills/bitlocker'), { recursive: true });
  await writeFile(join(home, 'settings.json'), JSON.stringify({ packages: [], defaultProjectTrust: 'never', defaultTools: ['read', 'ls'], retry: { enabled: false }, compaction: { enabled: false } }));
  await writeFile(join(home, 'models.json'), JSON.stringify({ providers: { fixture: { baseUrl: 'http://127.0.0.1/v1', api: 'openai-completions', apiKey: 'fixture-only', models: [{ id: 'fixture', name: 'fixture', reasoning: false, input: ['text'], contextWindow: 16000, maxTokens: 300 }] } } }));
  const key = `bitlocker-test-${randomUUID()}`;
  const state = { volumes: [volume()], calls: [], confirmed: [], updates: [], passwordInputs: 0, confirm: true, input: password };
  globalThis[Symbol.for(key)] = state;
  await writeFile(join(extension, 'bitlocker-windows.ts'), `
export function createBitLockerRunner(){return async (request,signal)=>{
 signal?.throwIfAborted();
 const state=globalThis[Symbol.for(${JSON.stringify(key)})];
 state.calls.push({...request,recoveryPassword:request.recoveryPassword===undefined?undefined:request.recoveryPassword===${JSON.stringify(password)}?'matched':'other'});
 if(state.runner)return state.runner(request,signal);
 const volumes=state.volumes.filter(v=>(!request.drive||v.drive===request.drive)&&(!request.volumeId||v.volumeId===request.volumeId));
 if(request.scope==='unlock')volumes.forEach(v=>v.lockStatus=0);
 if(request.scope==='decrypt')volumes.forEach(v=>v.conversionStatus=3);
 const reply={snapshot:{collectedAt:'2026-10-13T00:00:00.000Z',admin:state.admin!==false,volumes:structuredClone(volumes),collectionErrors:[]}};
 if(request.scope!=='status')reply.operation={status:'accepted',reason:'Windows 已接受请求',returnCode:0,error:null};
 return reply;
};}
`);
  await writeFile(join(home, 'extensions/probe.ts'), `
import {BitLockerWebInteraction,bitLockerWebInteractions} from './diagnostics/bitlocker-web.ts';
import {RecoveryPasswordInput} from './diagnostics/bitlocker-tui.ts';
import {CURSOR_MARKER} from '@earendil-works/pi-tui';
import {BITLOCKER_COMMAND} from './diagnostics/bitlocker-commands.ts';
import {parseBitLockerReply} from './diagnostics/bitlocker-data.ts';
import {createBitLockerRunner} from ${JSON.stringify(pathToFileURL(join(source, 'bitlocker-windows.ts')).href)};
export default function(){globalThis[Symbol.for(${JSON.stringify(key)})].api={BitLockerWebInteraction,bitLockerWebInteractions,RecoveryPasswordInput,CURSOR_MARKER,BITLOCKER_COMMAND,parseBitLockerReply,createBitLockerRunner};}
`);
  const old = process.env.PI_OFFLINE;
  process.env.PI_OFFLINE = '1';
  t.after(async () => {
    delete globalThis[Symbol.for(key)];
    if (old === undefined) delete process.env.PI_OFFLINE; else process.env.PI_OFFLINE = old;
    await rm(dir, { recursive: true, force: true });
  });
  const loader = new DefaultResourceLoader({ cwd: dir, agentDir: home, noContextFiles: true, noThemes: true, noPromptTemplates: true });
  await loader.reload();
  assert.deepEqual(loader.getExtensions().errors, []);
  const tools = loader.getExtensions().extensions.flatMap(e => [...e.tools.values()]).map(t => t.definition);
  const tool = tools.find(t => t.name === 'bitlocker');
  assert.ok(tool);
  const ui = {
    confirm: async (title, text) => { state.confirmed.push({ title, text }); return state.confirm; },
    custom: factory => new Promise(done => {
      state.passwordInputs++;
      const component = factory({ requestRender() {} }, theme, {}, done);
      if (state.input === undefined) component.handleInput('\x1b');
      else { component.handleInput(`\x1b[200~${state.input}\x1b[201~`); assert.ok(!component.render(80).join('\n').includes(password)); component.handleInput('\r'); }
    }),
  };
  const ctx = { cwd: dir, mode: 'tui', hasUI: true, ui };
  const run = async (params, options = {}) => {
    const result = await tool.execute(options.id || 'fixture-call', params, options.signal || new AbortController().signal, update => state.updates.push(update), options.ctx || ctx);
    const data = JSON.parse(result.content[0].text);
    assert.deepEqual(JSON.parse(JSON.stringify(result.details)), data);
    assert.ok(!JSON.stringify(result).includes(password));
    return data.bitlocker;
  };
  const emit = async (name, event) => {
    let last;
    for (const extension of loader.getExtensions().extensions) for (const handler of extension.handlers.get(name) || []) {
      const result = await handler(event, ctx);
      if (result?.message) event.message = result.message;
      if (result !== undefined) last = result;
    }
    return last;
  };
  return { state, loader, tool, tools, dir, home, run, ctx, emit };
}

test('Pi 发现八个工具与 bitlocker skill，参数和提示词可用', async t => {
  const h = await harness(t);
  assert.equal(h.tools.length, 8);
  assert.ok(h.tool.promptSnippet);
  assert.equal(h.tool.executionMode, 'sequential');
  assert.ok(h.loader.getSkills().skills.some(skill => skill.name === 'bitlocker'));
  const result = await h.run({});
  assert.equal(result.scope, 'status');
  assert.equal(result.volumes[0].lockStatus, 1);
  assert.equal(h.state.calls.length, 1);
  assert.equal(h.state.confirmed.length, 0);
});

test('TUI 自动主题跟随终端，BitLocker 标题与结果使用 Pi 工具文字配色', async t => {
  const h = await harness(t);
  const settings = JSON.parse(await readFile(join(root, 'agent/home/settings.json'), 'utf8'));
  assert.equal(settings.theme, 'light/dark');
  for (const appearance of ['light', 'dark']) {
    assert.equal(resolveThemeSetting(settings.theme, appearance), appearance);
    const current = getThemeByName(appearance);
    assert.ok(current);
    const call = h.tool.renderCall({ scope: 'status', drive: 'D:' }, current, {}).render(100).join('\n');
    assert.ok(call.includes(current.fg('toolOutput', 'status D:')));
    const result = { content: [{ type: 'text', text: 'fixture-output' }], details: undefined };
    const text = h.tool.renderResult(result, { expanded: false, isPartial: false }, current, {}).render(100).join('\n');
    assert.ok(text.includes(current.fg('toolOutput', 'fixture-output')));
  }
});

test('参数边界和缺失盘符不执行 Windows 操作', async t => {
  const h = await harness(t);
  for (const params of [{ scope: 'unlock' }, { drive: 'C:/Windows' }, { scope: 'status', recoveryPassword: password }, { scope: 'unlock', drive: 'D', recoveryPassword: 'wrong' }]) {
    await assert.rejects(h.run(params));
  }
  assert.equal(h.state.calls.length, 0);
  await assert.rejects(h.run({ drive: 'Z' }), /未找到/);
  assert.equal(h.state.calls.length, 1);
});

test('TUI 独立输入解锁，输出和工具更新均不包含凭据', async t => {
  const h = await harness(t);
  const result = await h.run({ scope: 'unlock', drive: 'd' });
  assert.equal(result.volumes[0].lockStatus, 0);
  assert.equal(result.volumes[0].conversionStatus, 1);
  assert.equal(result.volumes[0].operation.credentialSource, 'localInput');
  assert.equal(h.state.calls[1].recoveryPassword, 'matched');
  assert.equal(h.state.passwordInputs, 1);
  assert.ok(!JSON.stringify(h.state.updates).includes(password));
});

test('已解锁、用户取消、权限不足和非 TUI 模式均不提交解锁', async t => {
  const h = await harness(t);
  h.state.volumes[0].lockStatus = 0;
  assert.equal((await h.run({ scope: 'unlock', drive: 'D:' })).volumes[0].operation.status, 'skipped');
  assert.equal(h.state.passwordInputs, 0);
  h.state.volumes[0].lockStatus = 1;
  h.state.input = undefined;
  assert.equal((await h.run({ scope: 'unlock', drive: 'D:' })).volumes[0].operation.status, 'cancelled');
  h.state.admin = false;
  assert.equal((await h.run({ scope: 'unlock', drive: 'D:' })).volumes[0].operation.status, 'rejected');
  h.state.admin = true;
  assert.equal((await h.run({ scope: 'unlock', drive: 'D:', recoveryPassword: password }, { ctx: { ...h.ctx, mode: 'rpc' } })).volumes[0].operation.status, 'rejected');
  assert.ok(h.state.calls.every(call => call.scope === 'status'));
});

test('解密默认全卷，确认固定目标，跳过完成与进行中卷，逐卷保留失败', async t => {
  const h = await harness(t);
  h.state.volumes = [volume('C:', { lockStatus: 0 }), volume('D:', { lockStatus: 0, conversionStatus: 0 }), volume('E:', { lockStatus: 0, conversionStatus: 3 }), volume('F:'), volume('G:', { lockStatus: 0, conversionStatus: 5 })];
  h.state.runner = async request => {
    if (request.scope === 'status') return { snapshot: snapshot(h.state.volumes) };
    const current = h.state.volumes.find(v => v.volumeId === request.volumeId);
    return { snapshot: snapshot([current]), operation: current.drive === 'C:' ? { status: 'rejected', reason: '自动解锁依赖', returnCode: 0x80310029, error: { source: 'Decrypt', code: 0x80310029, message: 'blocked' } } : accepted };
  };
  const result = await h.run({ scope: 'decrypt' });
  assert.deepEqual(result.volumes.map(v => v.operation.status), ['rejected', 'skipped', 'skipped', 'rejected', 'accepted']);
  assert.equal(result.volumes[0].operation.returnCode, 0x80310029);
  assert.deepEqual(h.state.calls.filter(call => call.scope === 'decrypt').map(call => call.drive), ['C:', 'G:']);
  assert.equal(h.state.confirmed.length, 1);
  assert.match(h.state.confirmed[0].text, /fixture-C:/);
  assert.ok(!h.state.confirmed[0].text.includes('fixture-F:'));
});

test('指定盘符解密与取消确认，不包含其他卷', async t => {
  const h = await harness(t);
  h.state.volumes = [volume('C:', { lockStatus: 0 }), volume('D:', { lockStatus: 0 })];
  h.state.confirm = false;
  assert.equal((await h.run({ scope: 'decrypt', drive: 'd' })).volumes[0].operation.status, 'cancelled');
  assert.ok(h.state.calls.every(call => call.scope === 'status'));
  h.state.confirm = true;
  const result = await h.run({ scope: 'decrypt', drive: 'D:' });
  assert.equal(result.volumes.length, 1);
  assert.equal(result.volumes[0].conversionStatus, 3);
  assert.equal(h.state.calls.at(-1).drive, 'D:');
});

test('调用中断返回 unknown，复查失败不复用操作前状态，也不重复操作', async t => {
  const h = await harness(t);
  h.state.volumes[0].lockStatus = 0;
  let status = 0;
  h.state.runner = async request => {
    if (request.scope === 'status' && status++ === 0) return { snapshot: snapshot(h.state.volumes) };
    throw new Error(`fixture transport error ${password}`);
  };
  const result = await h.run({ scope: 'decrypt', drive: 'D:' });
  assert.equal(result.volumes[0].operation.status, 'unknown');
  assert.equal(result.volumes[0].conversionStatus, null);
  assert.equal(result.degraded, true);
  assert.equal(h.state.calls.filter(call => call.scope === 'decrypt').length, 1);
});

test('已接受但复查缺失保留 accepted，采集错误不会伪装为完成', async t => {
  const h = await harness(t);
  h.state.runner = async request => request.scope === 'status' ? { snapshot: snapshot(h.state.volumes) } : { snapshot: { ...snapshot([]), collectionErrors: [{ source: 'GetLockStatus', code: 5, message: 'denied' }] }, operation: accepted };
  const result = await h.run({ scope: 'unlock', drive: 'D:' });
  assert.equal(result.volumes[0].operation.status, 'accepted');
  assert.equal(result.volumes[0].lockStatus, null);
  assert.equal(result.degraded, true);
});

test('AI 参数在 finalized message 落盘前隐藏，按调用和盘符绑定，仅使用一次', async t => {
  const h = await harness(t);
  const assistant = { role: 'assistant', content: [{ type: 'toolCall', id: 'secret-call', name: 'bitlocker', arguments: { scope: 'unlock', drive: 'D:', recoveryPassword: password } }] };
  const replacement = await h.emit('message_end', { message: assistant });
  assert.ok(!JSON.stringify(replacement).includes(password));
  const args = replacement.message.content[0].arguments;
  assert.ok(!h.tool.renderCall(args, theme, {}).render(80).join('\n').includes(password));
  const result = await h.run(args, { id: 'secret-call' });
  assert.equal(result.volumes[0].operation.credentialSource, 'toolParameter');
  assert.equal(h.state.calls[1].recoveryPassword, 'matched');
  assert.equal(h.state.passwordInputs, 0);
  await assert.rejects(h.run(args, { id: 'secret-call' }), /已过期/);
});

test('TUI 隐藏输入支持粘贴、窄宽、显示切换、无效格式与中断', async t => {
  const h = await harness(t);
  const abort = new AbortController();
  let value = 'pending';
  const component = new h.state.api.RecoveryPasswordInput(volume(), theme, () => {}, input => value = input, abort.signal);
  component.handleInput('\x1b[200~wrong\x1b[201~');
  component.handleInput('\r');
  assert.equal(value, 'pending');
  assert.match(component.render(40).join('\n'), /八组六位/);
  component.handleInput('\x15'); // Ctrl+U
  component.handleInput(`\x1b[200~${password}\x1b[201~`);
  for (const width of [1, 8, 20, 80]) assert.ok(!component.render(width).join('\n').includes(password));
  const end = component.render(100).at(-1).indexOf(h.state.api.CURSOR_MARKER);
  component.handleInput('\x1b[D');
  assert.equal(component.render(100).at(-1).indexOf(h.state.api.CURSOR_MARKER), end - 1);
  component.handleInput('\x1b[C');
  component.handleInput('\x12'); // Ctrl+R
  assert.ok(component.render(100).join('\n').includes(password));
  component.handleInput('\x12');
  assert.ok(!component.render(100).join('\n').includes(password));
  abort.abort();
  assert.equal(value, undefined);
  assert.ok(!component.render(100).join('\n').includes(password));
});

test('同卷并发操作被拒绝，提交后中断保留结果并取消剩余卷', async t => {
  const h = await harness(t);
  const other = await harness(t);
  let entered;
  const waiting = new Promise(resolve => entered = resolve);
  let release;
  h.ctx.ui.custom = () => new Promise(resolve => { release = resolve; entered(); });
  const first = h.run({ scope: 'unlock', drive: 'D:' });
  await waiting;
  assert.equal((await other.run({ scope: 'unlock', drive: 'D:' })).volumes[0].operation.reason, '目标卷已有操作进行中');
  release(undefined);
  assert.equal((await first).volumes[0].operation.status, 'cancelled');
  const abort = new AbortController();
  h.state.volumes = [volume('C:', { lockStatus: 0 }), volume('E:', { lockStatus: 0 })];
  h.state.runner = async request => {
    if (request.scope === 'status') return { snapshot: snapshot(h.state.volumes) };
    abort.abort();
    throw new Error('interrupted after submit');
  };
  const result = await h.run({ scope: 'decrypt' }, { signal: abort.signal });
  assert.deepEqual(result.volumes.map(v => v.operation.status), ['unknown', 'cancelled']);
  assert.equal(h.state.calls.filter(call => call.scope === 'decrypt').length, 1);
});

test('凭据不能跨盘符使用，用户原文仅在本轮请求临时恢复，结束后清除', async t => {
  const h = await harness(t);
  const source = { role: 'user', content: [{ type: 'text', text: `unlock ${password}` }], timestamp: 1 };
  const safe = (await h.emit('message_end', { message: source })).message;
  assert.ok(!JSON.stringify(safe).includes(password));
  assert.ok(JSON.stringify(await h.emit('context', { messages: [safe] })).includes(password));
  await h.emit('agent_end', {});
  assert.ok(!JSON.stringify(await h.emit('context', { messages: [safe] })).includes(password));
  const assistant = { role: 'assistant', content: [{ type: 'toolCall', id: 'bound', name: 'bitlocker', arguments: { scope: 'unlock', drive: 'D:', recoveryPassword: password } }] };
  const args = (await h.emit('message_end', { message: assistant })).message.content[0].arguments;
  await assert.rejects(h.run({ ...args, drive: 'C:' }, { id: 'bound' }), /不匹配/);
  assert.equal(h.state.calls.length, 0);
});

test('真实 Pi 会话：模型实际调用 unlock，Windows 数据源替身执行，JSONL 无明文凭据', async t => {
  const h = await harness(t);
  const model = { id: 'fixture', name: 'fixture', api: 'openai-completions', provider: 'fixture', baseUrl: 'http://127.0.0.1', reasoning: false, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 16000, maxTokens: 300 };
  const { session } = await createAgentSession({ cwd: h.dir, agentDir: h.home, resourceLoader: h.loader, model, sessionManager: SessionManager.create(h.dir, join(h.home, 'sessions')), settingsManager: SettingsManager.create(h.dir, h.home) });
  t.after(() => session.dispose());
  await session.bindExtensions({ mode: 'tui', uiContext: h.ctx.ui });
  assert.ok(session.getActiveToolNames().includes('bitlocker'));
  let requests = 0;
  session.agent.streamFunction = (_model, context) => {
    const stream = createAssistantMessageEventStream();
    const first = requests++ === 0;
    if (first) {
      assert.ok(context.tools.some(tool => tool.name === 'bitlocker'), 'model receives the tool declaration');
      assert.ok(JSON.stringify(context.messages).includes(password), 'AI path gets a request-local copy');
    }
    const message = { role: 'assistant', content: first ? [{ type: 'toolCall', id: 'session-secret', name: 'bitlocker', arguments: { scope: 'unlock', drive: 'D:', recoveryPassword: password } }] : [{ type: 'text', text: 'DONE' }], api: model.api, provider: model.provider, model: model.id, usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: first ? 'toolUse' : 'stop', timestamp: Date.now() };
    queueMicrotask(() => { stream.push({ type: 'done', reason: message.stopReason, message }); stream.end(); });
    return stream;
  };
  await session.prompt(`请使用恢复密码解锁 D: ${password}`);
  assert.equal(requests, 2);
  assert.equal(h.state.calls[1].recoveryPassword, 'matched');
  const data = await readFile(session.sessionManager.getSessionFile(), 'utf8');
  assert.ok(!data.includes(password));
  assert.ok(data.includes('toolParameter'));
});

test('生产子进程传参使用 stdin，argv 和返回值不含恢复密码', { skip: process.platform !== 'win32' || !existsSync(join(root, 'pwsh/pwsh.exe')) }, async t => {
  const h = await harness(t);
  const native = childProcess.spawn;
  let captured = false;
  t.mock.method(childProcess, 'spawn', (path, args, options) => {
    captured = true;
    assert.ok(!args.join(' ').includes(password));
    assert.ok(args.at(-1).includes('[Console]::In.ReadToEnd()'));
    const output = JSON.stringify({ snapshot: snapshot([volume('D:', { lockStatus: 0 })]), operation: accepted });
    const fake = `$r=[Console]::In.ReadToEnd()|ConvertFrom-Json; if($r.recoveryPassword.Length -ne 55){exit 1}; Write-Output '${output}'`;
    return native(path, [...args.slice(0, -1), fake], options);
  });
  syncBuiltinESMExports();
  t.after(() => { t.mock.restoreAll(); syncBuiltinESMExports(); });
  const reply = await h.state.api.createBitLockerRunner()({ scope: 'unlock', drive: 'D:', volumeId: 'fixture-D:', recoveryPassword: password });
  assert.equal(captured, true);
  assert.equal(reply.operation.status, 'accepted');
  assert.ok(!JSON.stringify(reply).includes(password));
});

async function pwsh(program, request) {
  return new Promise((resolve, reject) => {
    const child = spawn(join(root, 'pwsh/pwsh.exe'), ['-NoProfile', '-NonInteractive', '-Command', program], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    let out = '', err = '';
    child.stdout.on('data', chunk => out += chunk);
    child.stderr.on('data', chunk => err += chunk);
    child.on('error', reject);
    child.on('close', code => code === 0 ? resolve(JSON.parse(out)) : reject(new Error(err)));
    child.stdin.end(JSON.stringify(request));
  });
}

test('生产 DCOM 选项可创建且启用 PacketPrivacy，无需管理员或真实卷', { skip: process.platform !== 'win32' || !existsSync(join(root, 'pwsh/pwsh.exe')) }, async t => {
  const h = await harness(t);
  const line = h.state.api.BITLOCKER_COMMAND.match(/^  \$option = .+$/m)[0];
  const value = await pwsh(`$ErrorActionPreference='Stop'; ${line}; @{packetPrivacy=$option.PacketPrivacy}|ConvertTo-Json`, {});
  assert.equal(value.packetPrivacy, true);
});

test('真实 PowerShell 执行静态程序：仅替换 CIM 命令，核查返回码、复查与盘符变化', { skip: process.platform !== 'win32' || !existsSync(join(root, 'pwsh/pwsh.exe')), timeout: 30000 }, async t => {
  const h = await harness(t);
  const prefix = `
function New-CimSessionOption { param($Protocol,[switch]$PacketPrivacy) return @{PacketPrivacy=$true} }
function New-CimSession { param($ComputerName,$SessionOption) return @{} }
function Remove-CimSession { param($ErrorAction,$CimSession) }
function Get-CimInstance { param($CimSession,$Namespace,$ClassName) return [pscustomobject]@{DeviceID='fixture-D:';DriveLetter='D:';VolumeType=1} }
$script:lock = 1
function Invoke-CimMethod {
 param($CimSession,$InputObject,$MethodName,$Arguments)
 switch ($MethodName) {
 'GetLockStatus' { return [pscustomobject]@{ReturnValue=0;LockStatus=$script:lock} }
 'GetConversionStatus' { return [pscustomobject]@{ReturnValue=0;ConversionStatus=1;EncryptionPercentage=100} }
 'GetEncryptionMethod' { return [pscustomobject]@{ReturnValue=0;EncryptionMethod=6} }
 'GetProtectionStatus' { return [pscustomobject]@{ReturnValue=0;ProtectionStatus=1} }
 'IsAutoUnlockEnabled' { return [pscustomobject]@{ReturnValue=0;IsAutoUnlockEnabled=$false} }
 'GetKeyProtectors' { return [pscustomobject]@{ReturnValue=0;VolumeKeyProtectorID=@('fixture-id')} }
 'GetKeyProtectorType' { return [pscustomobject]@{ReturnValue=0;KeyProtectorType=3} }
 'UnlockWithNumericalPassword' { if($Arguments.NumericalPassword -ne '${password}') {throw 'wrong fixture input'}; $script:lock=0; return [pscustomobject]@{ReturnValue=0} }
 'Decrypt' { return [pscustomobject]@{ReturnValue=[uint32]2150694953} }
 default { throw 'Unexpected method' }
 }
}
`;
  const program = prefix + h.state.api.BITLOCKER_COMMAND.replace(/^\$admin = .*$/m, '$admin = $true');
  const unlocked = h.state.api.parseBitLockerReply(await pwsh(program, { scope: 'unlock', drive: 'D:', volumeId: 'fixture-D:', recoveryPassword: password }));
  assert.equal(unlocked.operation.status, 'accepted');
  assert.equal(unlocked.snapshot.volumes[0].lockStatus, 0);
  assert.equal(unlocked.snapshot.volumes[0].keyProtectors[0].type, 3);
  const moved = await pwsh(program, { scope: 'unlock', drive: 'E:', volumeId: 'fixture-D:', recoveryPassword: password });
  assert.equal(moved.operation.status, 'rejected');
  assert.match(moved.operation.reason, /盘符已变化/);
  const decrypt = await pwsh(program.replace('$script:lock = 1', '$script:lock = 0'), { scope: 'decrypt', drive: 'D:', volumeId: 'fixture-D:' });
  assert.equal(decrypt.operation.returnCode, 0x80310029);
  assert.equal(decrypt.operation.status, 'rejected');
  const partial = await pwsh(program.replace('ReturnValue=0;ProtectionStatus=1', 'ReturnValue=5;ProtectionStatus=1'), { scope: 'status', drive: 'D:' });
  assert.equal(partial.snapshot.volumes[0].protectionStatus, null);
  assert.equal(partial.snapshot.volumes[0].collectionErrors[0].code, 5);
  const locked = await pwsh(program, { scope: 'decrypt', drive: 'D:', volumeId: 'fixture-D:' });
  assert.equal(locked.operation.returnCode, null);
  assert.equal(locked.operation.reason, '卷已锁定');
  const failed = await pwsh(program.replace('$script:lock=0; return [pscustomobject]@{ReturnValue=0}', `throw '${password}'`), { scope: 'unlock', drive: 'D:', volumeId: 'fixture-D:', recoveryPassword: password });
  assert.equal(failed.operation.status, 'unknown');
  assert.ok(!JSON.stringify(failed).includes(password));
  assert.match(failed.operation.error.message, /已隐藏/);
});

test('Web 模式使用独立输入通道，固定目标解密，凭据不进入结果和更新', async t => {
  const h = await harness(t);
  const { BitLockerWebInteraction, bitLockerWebInteractions } = h.state.api;
  const bridge = new BitLockerWebInteraction();
  const id = randomUUID();
  bitLockerWebInteractions.set(id, bridge);
  t.after(() => { bridge.close(); bitLockerWebInteractions.delete(id); });
  const ctx = { ...h.ctx, mode: 'rpc', sessionManager: { getSessionId: () => id } };
  const pending = h.run({ scope: 'unlock', drive: 'D:' }, {ctx});
  for (let i=0; !bridge.snapshot().length && i<50; i++) await new Promise(resolve=>setTimeout(resolve,5));
  const request = bridge.snapshot()[0];
  assert.equal(request.kind,'password');
  bridge.respond(request.requestId,{password});
  assert.equal((await pending).volumes[0].operation.status,'accepted');
  assert.equal(h.state.passwordInputs,0);
  assert.ok(!JSON.stringify(h.state.updates).includes(password));
  const decrypt = h.run({scope:'decrypt',drive:'D:'},{ctx});
  for (let i=0; !bridge.snapshot().length && i<50; i++) await new Promise(resolve=>setTimeout(resolve,5));
  const confirmation = bridge.snapshot()[0];
  assert.equal(confirmation.kind,'decrypt');
  assert.equal(confirmation.targets.length,1);
  bridge.respond(confirmation.requestId,{confirm:true});
  assert.equal((await decrypt).volumes[0].conversionStatus,3);
});
