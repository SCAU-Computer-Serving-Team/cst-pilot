import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { checkReleaseContent, checkReleaseTree, REQUIRED_RELEASE_FILES } from '../release-checks.mjs';
import { RELEASE_TELEMETRY, RELEASE_WEB_SEARCH } from '../release-settings.mjs';

test('Web backend tests are rejected from release trees', () => {
  assert.throws(
    () => checkReleaseTree('.', ['agent/home/extensions/web/test/http.test.ts']),
    /Web 测试文件不得进入发行包/,
  );
});

test("项目AGENTS.md不得进入任何发行目录",()=>{for(const file of ["AGENTS.md","doc/AGENTS.md","agent/home/agents.md"]){assert.throws(()=>checkReleaseTree(".",[file]),/AGENTS.md/);}});

test('遥测测试和待发队列不得进入发行树', () => {
  assert.throws(()=>checkReleaseTree('.', ['agent/home/extensions/telemetry/test/sender.test.ts']), /Telemetry 测试文件/);
  assert.throws(()=>checkReleaseTree('.', ['agent/home/telemetry/pending-target.jsonl']), /运行状态或凭据路径/);
});

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

test('only the generated credential-free web-search policy enters a release', () => {
  const root=makeTree(),file='agent/home/web-search.json';
  try {
    fs.writeFileSync(path.join(root,file),JSON.stringify(RELEASE_WEB_SEARCH));
    assert.doesNotThrow(()=>checkReleaseTree(root,[file]));
    for(const value of [{ssrf:{allowRanges:['0.0.0.0/0']}},{...RELEASE_WEB_SEARCH,proxy:'http://local-proxy'},{...RELEASE_WEB_SEARCH,apiKey:'do-not-distribute'},{ssrf:{allowRanges:['198.18.0.0/15'],trustEnvProxy:true}},{},[]]) {
      fs.writeFileSync(path.join(root,file),JSON.stringify(value));
      assert.throws(()=>checkReleaseTree(root,[file]),/必须严格匹配/);
    }
  } finally { fs.rmSync(root,{recursive:true,force:true}); }
});

test('a release without its generated TUN policy is rejected',()=>{
  const root=makeTree();
  try{fs.rmSync(path.join(root,'agent/home/web-search.json'));assert.throws(()=>checkReleaseContent(root,filesOf(root)),/缺少必需文件.*web-search/);}finally{fs.rmSync(root,{recursive:true,force:true});}
});

test('发行遥测配置固定双端，CA 文件缺失时拒绝发行', () => {
  const root=makeTree(), file='agent/home/telemetry.json';
  try {
    fs.writeFileSync(path.join(root,file), JSON.stringify(RELEASE_TELEMETRY));
    assert.doesNotThrow(()=>checkReleaseTree(root,[file]));
    fs.writeFileSync(path.join(root,file), JSON.stringify({...RELEASE_TELEMETRY,endpoints:RELEASE_TELEMETRY.endpoints.slice(0,1)}));
    assert.throws(()=>checkReleaseTree(root,[file]), /双端上报配置/);
    fs.rmSync(path.join(root,'agent/home/extensions/telemetry/timserver_1.crt'));
    assert.throws(()=>checkReleaseContent(root,filesOf(root)), /缺少必需文件.*timserver_1/);
  } finally {fs.rmSync(root,{recursive:true,force:true});}
});

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
