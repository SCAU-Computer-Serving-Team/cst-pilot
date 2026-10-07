import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { checkReleaseContent, checkReleaseTree, REQUIRED_RELEASE_FILES } from '../release-checks.mjs';

test('Web backend tests are rejected from release trees', () => {
  assert.throws(
    () => checkReleaseTree('.', ['agent/home/extensions/web/test/http.test.ts']),
    /Web 测试文件不得进入发行包/,
  );
});

test("项目AGENTS.md不得进入任何发行目录",()=>{for(const file of ["AGENTS.md","doc/AGENTS.md","agent/home/agents.md"]){assert.throws(()=>checkReleaseTree(".",[file]),/AGENTS.md/);}});

test('完整 OTF 字体不得进入 Web 发行树', () => {
  assert.throws(() => checkReleaseTree('.', ['agent/home/extensions/web/static/fonts/SourceHanSansCN-Regular.otf']), /完整 OTF 不得进入 Web 分发包/);
});

// 造一棵最小发行树：必需文件全部就位，入口页与样式表各自引用一个真实产物。
function makeTree() {
  const base = process.env.CST_WEB_TEST_TMPDIR ? path.join(process.env.CST_WEB_TEST_TMPDIR, new Date().toISOString().slice(0, 10)) : os.tmpdir();
  fs.mkdirSync(base, { recursive: true });
  const root = fs.mkdtempSync(path.join(base, 'cst-release-'));
  const write = (rel, body) => {
    const file = path.join(root, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, body);
  };
  for (const rel of REQUIRED_RELEASE_FILES) write(rel, 'x');
  write(
    'agent/home/extensions/web/static/index.html',
    '<link rel="stylesheet" href="/assets/styles-abc.css" /><script src="/assets/extra.js"></script>',
  );
  write('agent/home/extensions/web/static/assets/extra.js', 'x');
  write(
    'agent/home/extensions/web/static/assets/styles-abc.css',
    '@font-face{src:url(/fonts/CSTUISans-Regular.woff2)}',
  );
  return root;
}

const filesOf = (root) => {
  const out = [];
  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else out.push(path.relative(root, full).replaceAll('\\', '/'));
    }
  };
  walk(root);
  return out.sort();
};

test('a complete release tree passes the content check', () => {
  const root = makeTree();
  assert.doesNotThrow(() => checkReleaseContent(root, filesOf(root)));
  fs.rmSync(root, { recursive: true, force: true });
});

test('a missing license file is rejected', () => {
  const root = makeTree();
  fs.rmSync(path.join(root, 'licenses/MPL-2.0.txt'));
  assert.throws(() => checkReleaseContent(root, filesOf(root)), /发行包缺少必需文件: licenses\/MPL-2.0.txt/);
  fs.rmSync(root, { recursive: true, force: true });
});

test('a referenced asset missing from the tree is rejected', () => {
  const root = makeTree();
  fs.rmSync(path.join(root, 'agent/home/extensions/web/static/assets/extra.js'));
  assert.throws(() => checkReleaseContent(root, filesOf(root)), /引用的资源不在发行树中/);
  fs.rmSync(root, { recursive: true, force: true });
});
