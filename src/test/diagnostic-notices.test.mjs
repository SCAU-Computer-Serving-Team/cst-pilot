import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';
import { DefaultResourceLoader } from '../../agent/node_modules/@earendil-works/pi-coding-agent/dist/index.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const diagnostics = join(root, 'agent/home/extensions/diagnostics');
const unwanted = /直接信号|用降频信号替代|IO 瓶颈|说明物理内存不足|驱动泄漏|优先看|先排查|请联网比对|传感器已包含|真实显卡以/;

// Execute the registered tools; only the PowerShell data-source boundary is replaced.
async function harness(t) {
  const base = process.env.CST_WEB_TEST_TMPDIR || (process.platform === 'win32'
    ? (existsSync('E:/') ? 'E:/tmp' : 'F:/tmp') : tmpdir());
  const dated = join(base, new Date().toISOString().slice(0, 10));
  await mkdir(dated, { recursive: true });
  const kit = await mkdtemp(join(dated, 'diagnostic-notices-'));
  const home = join(kit, 'agent/home');
  const extension = join(home, 'extensions/diagnostics');
  await mkdir(extension, { recursive: true });
  await cp(diagnostics, extension, { recursive: true });
  await mkdir(join(kit, 'lhm'), { recursive: true });
  await writeFile(join(kit, 'lhm/LibreHardwareMonitorLib.dll'), 'fixture-only');
  await writeFile(join(home, 'settings.json'), JSON.stringify({ defaultProjectTrust: 'never', packages: [] }));
  const fixture = join(kit, 'fixture.json');
  await writeFile(fixture, '{}');
  await writeFile(join(extension, 'runtime.ts'), `
import {readFileSync} from 'node:fs';
export * from ${JSON.stringify(pathToFileURL(join(diagnostics, 'runtime.ts')).href)};
export const ROOT_DIR=${JSON.stringify(kit)};
export const PWSH='fixture-only';
export function createPwshRunner(){return async function(command, invocation={}){
 invocation.signal?.throwIfAborted();
 const data=JSON.parse(readFileSync(${JSON.stringify(fixture)},'utf8'));
 const key=command.includes('$isAdmin =')?'sensor':command.includes('engineSamples')?'gpu':command.includes('cpuTotalPct')?'overview':command.includes('queueLen')?'io':command.includes('byCpu')?'proc':command.includes('regItems')?'startup':command.includes('connStatus')?'core':command.includes('IsCpuEnabled = $false')?'lhmGpu':null;
 if(!key || !Object.hasOwn(data,key))throw new Error('Unexpected collection command');
 return data[key];
};}
`);
  const previous = { offline: process.env.PI_OFFLINE, windir: process.env.WINDIR };
  process.env.PI_OFFLINE = '1';
  process.env.WINDIR = join(kit, 'no-windows');
  t.after(async () => {
    for (const [name, value] of [['PI_OFFLINE', previous.offline], ['WINDIR', previous.windir]]) {
      if (value === undefined) delete process.env[name]; else process.env[name] = value;
    }
    await rm(kit, { recursive: true, force: true });
  });
  const loader = new DefaultResourceLoader({ cwd: kit, agentDir: home, noContextFiles: true, noSkills: true, noThemes: true, noPromptTemplates: true });
  await loader.reload();
  const loaded = loader.getExtensions();
  assert.deepEqual(loaded.errors, []);
  const tools = loaded.extensions.flatMap(extension => [...extension.tools.values()]).map(tool => tool.definition);
  return async (name, params, source) => {
    await writeFile(fixture, JSON.stringify(source));
    const tool = tools.find(tool => tool.name === name);
    assert.ok(tool);
    const result = await tool.execute('notice-regression', params, new AbortController().signal, undefined, { cwd: kit });
    const payload = JSON.parse(result.content.find(part => part.type === 'text').text);
    const data = payload[params.scope || name];
    assert.ok(data);
    assert.doesNotMatch(data.notice, unwanted);
    assert.deepEqual(JSON.parse(JSON.stringify(result.details)), payload);
    return data;
  };
}

test('传感器说明保留数据、权限和缺失，不根据驱动检测推断可读项', async t => {
  const run = await harness(t);
  const input = {
    admin: true, pawnio: true, hardware: ['fixture CPU'], sensors: [], thermalZones: [],
    frequency: { cores: 16, minPctOfMax: 30, avgPctOfMax: 35 },
    counterErrors: { thermal: 'fixture access denied', frequency: null },
    degraded: true, collectionErrors: [{ source: 'fixture', message: 'fixture denied' }],
  };
  const output = await run('sys', { scope: 'sensor' }, { sensor: input });
  for (const key of ['admin', 'pawnio', 'hardware', 'sensors', 'thermalZones', 'frequency', 'counterErrors', 'degraded', 'collectionErrors']) {
    assert.deepEqual(output[key], input[key]);
  }
  assert.equal(output.sensorCount, 0);
  assert.match(output.notice, /°C、RPM、V/);
  assert.match(output.notice, /最多返回 200 项/);
  assert.match(output.notice, /fixture access denied/);
  assert.match(output.notice, /可读项以 sensors 为准/);
  assert.doesNotMatch(output.notice, /过热|功耗降频|不可得/);
  input.admin = false;
  input.pawnio = false;
  input.degraded = false;
  input.counterErrors.thermal = null;
  input.sensors = [{ hw: 'fixture GPU', name: 'GPU Core', type: 'Temperature', value: 51 }];
  const normal = await run('sys', { scope: 'sensor' }, { sensor: input });
  assert.equal(normal.admin, false);
  assert.deepEqual(normal.sensors, input.sensors);
  assert.equal(normal.sensorCount, 1);
  assert.equal(normal.counterErrors, undefined);
});

test('负载、自启和设备返回说明保留口径与限制，不给出诊断阈值和排查顺序', async t => {
  const run = await harness(t);
  const cases = [
    ['sys', 'overview', { cpuTotalPct: 95, mem: { usedPct: 95 }, pagefile: [], pool: {}, machine: {}, uptime: {} }],
    ['sys', 'io', { intervalSec: 1.2, totalProcs: 1, disks: [{ busyPct: 99, queueLen: 8 }], byIo: [] }],
    ['sys', 'proc', { intervalSec: 1.2, cores: 16, totalProcs: 1, byCpu: [], byMem: [] }],
    ['startup', 'startup', { regItems: [{ disabled: true, command: 'fixture command' }], services: [], startupFolders: [] }],
    ['driver', 'core', { net: [], bluetooth: [], audio: [], display: [], services: [], drivers: [] }],
    ['sys', 'gpu', { engineSamples: 1, adapters: [{ name: 'fixture virtual display', bus: 'PCI', vendor: 'fixture' }], byGpuPct: [], byDedicatedMB: [], counterErrors: { engine: null, memory: null } }],
  ];
  for (const [name, scope, source] of cases) {
    const output = await run(name, name === 'startup' ? {} : { scope }, { [scope]: source, lhmGpu: { hardware: [], sensors: [] } });
    for (const [key, value] of Object.entries(source)) assert.deepEqual(output[key], value);
  }
});
