import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import ts from '../../agent/node_modules/typescript/lib/typescript.js';
import { createAgentSession, DefaultResourceLoader, SessionManager } from '../../agent/node_modules/@earendil-works/pi-coding-agent/dist/index.js';
import { testRoot } from '../../agent/home/extensions/web/test/support.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const json = async path => JSON.parse(await readFile(join(root, path), 'utf8'));

test('MJS、JSON、CSS 的错误语法进入静态检查，不触发整仓格式化', async () => {
  const biome = join(root, 'agent/node_modules/@biomejs/biome/bin/biome');
  for (const [file, source] of [
    ['src/check/invalid.mjs', 'const value = ;'],
    ['pack/invalid.json', '{"key": 1, "key": 2}'],
    ['src/web/frontend/app/invalid.css', '.x { color: red; color: blue; }'],
  ]) {
    const result = spawnSync(process.execPath, [biome, 'lint', '--stdin-file-path', file], { cwd: root, input: source, encoding: 'utf8' });
    assert.equal(result.error, undefined);
    assert.notEqual(result.status, 0, file);
    assert.doesNotMatch(result.stderr, /no files were processed/i, file);
  }
  const config = await json('biome.json');
  assert.ok(config.overrides.some(rule => rule.includes.includes('**/*.mjs') && rule.formatter.enabled === false));
});

test('直接依赖固定版本，清单与锁文件一致', async () => {
  for (const [manifest, lockfile] of [
    ['agent/package.json', 'agent/package-lock.json'],
    ['src/web/frontend/package.json', 'src/web/frontend/package-lock.json'],
    ['pack/extensions.package.json', 'pack/extensions.package-lock.json'],
  ]) {
    const pkg = await json(manifest);
    const lock = await json(lockfile);
    if (pkg.engines) assert.deepEqual(pkg.engines, lock.packages[''].engines, `${manifest}: engines`);
    for (const field of ['dependencies', 'devDependencies', 'optionalDependencies']) {
      assert.deepEqual(pkg[field] ?? {}, lock.packages[''][field] ?? {}, `${manifest}: ${field}`);
      for (const [name, version] of Object.entries(pkg[field] ?? {})) {
        assert.match(version, /^\d+\.\d+\.\d+(?:-[\da-zA-Z.-]+)?$/, `${manifest}: ${name}`);
        assert.equal(lock.packages[`node_modules/${name}`]?.version, version, `${lockfile}: ${name}`);
      }
    }
  }
});

test('所有扩展的 TypeScript 文件均纳入严格类型检查', async () => {
  const configFile = join(root, 'agent/tsconfig.json');
  const loaded = ts.readConfigFile(configFile, ts.sys.readFile);
  assert.equal(loaded.error, undefined);
  const parsed = ts.parseJsonConfigFileContent(loaded.config, ts.sys, dirname(configFile));
  assert.deepEqual(parsed.errors, []);
  assert.equal(parsed.options.strict, true);
  assert.equal(parsed.options.erasableSyntaxOnly, true);
  const included = new Set(parsed.fileNames.map(file => resolve(file).toLowerCase()));
  const missing = [];
  async function walk(directory) {
    for (const item of await readdir(directory, { withFileTypes: true })) {
      if (item.name === 'node_modules' || item.name === 'static') continue;
      const file = join(directory, item.name);
      if (item.isDirectory()) await walk(file);
      else if (item.name.endsWith('.ts') && !included.has(resolve(file).toLowerCase())) {
        missing.push(relative(root, file));
      }
    }
  }
  await walk(join(root, 'agent/home/extensions'));
  assert.deepEqual(missing, []);
});

test('开发环境声明满足锁定 Pi SDK 的 Node 下限', async () => {
  const sdk = await json('agent/node_modules/@earendil-works/pi-coding-agent/package.json');
  for (const path of ['agent/package.json', 'src/web/frontend/package.json']) {
    assert.equal((await json(path)).engines.node, sdk.engines.node, path);
  }
});

test('项目 TypeScript 使用顶层导入，不隐藏动态模块依赖', async () => {
  const failures = [];
  async function walk(directory) {
    for (const item of await readdir(directory, { withFileTypes: true })) {
      if (['node_modules', 'static', 'build', '.react-router'].includes(item.name)) continue;
      const file = join(directory, item.name);
      if (item.isDirectory()) await walk(file);
      else if (/\.tsx?$/.test(item.name)) {
        const source = ts.createSourceFile(file, await readFile(file, 'utf8'), ts.ScriptTarget.Latest, true);
        const visit = node => {
          if (ts.isImportTypeNode(node) || (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword)) {
            failures.push(`${relative(root, file)}:${source.getLineAndCharacterOfPosition(node.getStart()).line + 1}`);
          }
          ts.forEachChild(node, visit);
        };
        visit(source);
      }
    }
  }
  await walk(join(root, 'agent/home/extensions'));
  await walk(join(root, 'src/web/frontend/app'));
  assert.deepEqual(failures, []);
});

test('运行协调尊重 Pi 的自定义模型与提交快捷键', { timeout: 30_000 }, async t => {
  const temporary = await mkdtemp(join(await testRoot(), 'engineering-keybindings-'));
  const home = join(temporary, 'home');
  await mkdir(home);
  await writeFile(join(home, 'settings.json'), JSON.stringify({ packages: [], defaultProjectTrust: 'never' }));
  const probe = join(temporary, 'keybindings.ts');
  const key = `cst-engineering-${crypto.randomUUID()}`;
  await writeFile(probe, `
import { getKeybindings, setKeybindings, KeybindingsManager } from '@earendil-works/pi-tui';
export default function () {
  globalThis[Symbol.for(${JSON.stringify(key)})] = { KeybindingsManager, getKeybindings, setKeybindings };
}
`);
  const oldHome = process.env.PI_CODING_AGENT_DIR;
  const oldOffline = process.env.PI_OFFLINE;
  process.env.PI_CODING_AGENT_DIR = home;
  process.env.PI_OFFLINE = '1';
  let session;
  let bindings;
  let previousBindings;
  const initialDataListeners = new Set(process.stdin.listeners('data'));
  t.after(async () => {
    session?.dispose();
    if (bindings && previousBindings) bindings.setKeybindings(previousBindings);
    for (const listener of process.stdin.listeners('data')) {
      if (!initialDataListeners.has(listener)) process.stdin.removeListener('data', listener);
    }
    process.stdin.pause();
    process.stdin.unref?.();
    delete globalThis[Symbol.for(key)];
    if (oldHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
    else process.env.PI_CODING_AGENT_DIR = oldHome;
    if (oldOffline === undefined) delete process.env.PI_OFFLINE;
    else process.env.PI_OFFLINE = oldOffline;
    await rm(temporary, { recursive: true, force: true });
  });
  const loader = new DefaultResourceLoader({
    cwd: temporary, agentDir: home, noContextFiles: true, noExtensions: true,
    noSkills: true, noThemes: true, noPromptTemplates: true,
    additionalExtensionPaths: [join(root, 'agent/home/extensions/runtime/index.ts'), probe],
  });
  await loader.reload();
  assert.deepEqual(loader.getExtensions().errors, []);
  ({ session } = await createAgentSession({
    cwd: temporary, agentDir: home, resourceLoader: loader,
    sessionManager: SessionManager.inMemory(temporary), tools: [],
  }));
  let input;
  let editor = '';
  const notifications = [];
  await session.bindExtensions({ mode: 'tui', uiContext: {
    onTerminalInput(handler) { input = handler; return () => { input = undefined; }; },
    getEditorText() { return editor; },
    notify(message) { notifications.push(message); },
  } });
  assert.equal(typeof input, 'function');
  bindings = globalThis[Symbol.for(key)];
  previousBindings = bindings.getKeybindings();
  bindings.setKeybindings(new bindings.KeybindingsManager({
    'app.model.select': { defaultKeys: 'ctrl+k' },
    'app.model.cycleForward': { defaultKeys: 'ctrl+n' },
    'app.model.cycleBackward': { defaultKeys: 'ctrl+b' },
    'tui.input.submit': { defaultKeys: 'ctrl+s' },
  }));
  for (const data of ['\x0b', '\x0e', '\x02']) {
    const replayed = once(process.stdin, 'data');
    assert.deepEqual(input(data), { consume: true });
    assert.deepEqual(await replayed, [data]);
  }
  assert.equal(input('\x0c'), undefined, '被改绑的旧模型键不拦截');
  editor = '/model';
  assert.equal(input('\r'), undefined, '被改绑的旧提交键不拦截');
  const replayed = once(process.stdin, 'data');
  assert.deepEqual(input('\x13'), { consume: true });
  assert.deepEqual(await replayed, ['\x13']);
  assert.deepEqual(notifications, []);
});
