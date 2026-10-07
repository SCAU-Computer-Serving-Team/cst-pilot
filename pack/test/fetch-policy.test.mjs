import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { RELEASE_WEB_SEARCH } from '../release-settings.mjs';

// Node不直接剥离node_modules中的TS；原样复制固定版本模块到隔离目录后执行。
const base = path.join(
  process.env.CST_WEB_TEST_TMPDIR ?? (fs.existsSync('E:/') ? 'E:/tmp' : fs.existsSync('F:/') ? 'F:/tmp' : os.tmpdir()),
  new Date().toISOString().slice(0, 10),
);
fs.mkdirSync(base, { recursive: true });
const root = fs.mkdtempSync(path.join(base, 'cst-fetch-policy-'));
const source = fileURLToPath(new URL('../../agent/home/npm/node_modules/pi-web-access/', import.meta.url));
const lock = JSON.parse(fs.readFileSync(new URL('../extensions.package-lock.json', import.meta.url)));
assert.equal(JSON.parse(fs.readFileSync(path.join(source, 'package.json'))).version, lock.packages['node_modules/pi-web-access'].version);
for (const name of ['ssrf-protection.ts', 'utils.ts']) {
  fs.copyFileSync(path.join(source, name), path.join(root, name));
}
const previous = process.env.PI_CODING_AGENT_DIR;
process.env.PI_CODING_AGENT_DIR = root;
const { fetchRemoteUrl, validateRemoteUrl, loadSsrfConfig } = await import(pathToFileURL(path.join(root, 'ssrf-protection.ts')).href);
after(() => {
  if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
  else process.env.PI_CODING_AGENT_DIR = previous;
  fs.rmSync(root, { recursive: true, force: true });
});

const url = 'https://www.nvidia.cn/geforce/news/call-of-duty-modern-warfare-4-star-wars-galactic-racer-game-ready-driver/';
const policy = RELEASE_WEB_SEARCH.ssrf;
const lookup = async () => [{ address: '198.18.0.222', family: 4 }];

test('截图中的NVIDIA网址通过发行TUN规则，不再因假IP被拦截', async () => {
  await assert.rejects(validateRemoteUrl(url, { lookup }), /Blocked internal address.*198\.18\.0\.222/);
  const checked = await validateRemoteUrl(url, { ...policy, lookup });
  assert.equal(checked.href, url);
  let requests = 0;
  const response = await fetchRemoteUrl(url, {}, {
    ...policy,
    lookup,
    fetch: async () => {
      requests++;
      return new Response('NVIDIA_PUBLIC_PAGE', { status: 200 });
    },
  });
  assert.equal(await response.text(), 'NVIDIA_PUBLIC_PAGE');
  assert.equal(requests, 1);
});

test('发行配置由真实网页扩展读取，只放行198.18/15，不信任环境代理', () => {
  fs.writeFileSync(path.join(root, 'web-search.json'), JSON.stringify(RELEASE_WEB_SEARCH));
  assert.deepEqual(loadSsrfConfig(), policy);
  assert.deepEqual(RELEASE_WEB_SEARCH, { ssrf: { allowRanges: ['198.18.0.0/15'], trustEnvProxy: false } });
});

test('假IP段两端放行，其他私网、localhost和混合DNS继续拦截', async () => {
  for (const address of ['198.18.0.0', '198.19.255.255']) {
    await validateRemoteUrl(url, { ...policy, lookup: async () => [{ address, family: 4 }] });
  }
  for (const address of ['127.0.0.1', '10.0.0.1', '172.16.0.1', '192.168.1.1', '169.254.169.254', '::1', 'fc00::1']) {
    await assert.rejects(
      validateRemoteUrl(url, { ...policy, lookup: async () => [{ address, family: address.includes(':') ? 6 : 4 }] }),
      /Blocked internal address/,
    );
  }
  await assert.rejects(validateRemoteUrl('http://localhost/', policy), /Blocked internal hostname/);
  await assert.rejects(validateRemoteUrl(url, {
    ...policy,
    lookup: async () => [{ address: '198.18.0.222', family: 4 }, { address: '192.168.1.1', family: 4 }],
  }), /Blocked internal address/);
});

test('经假IP抓取后的私网重定向在发出下一请求前拒绝', async () => {
  let requests = 0;
  await assert.rejects(fetchRemoteUrl(url, {}, {
    ...policy,
    lookup,
    fetch: async () => {
      requests++;
      return new Response(null, { status: 302, headers: { Location: 'http://127.0.0.1/private' } });
    },
  }), /Blocked internal address/);
  assert.equal(requests, 1);
});
