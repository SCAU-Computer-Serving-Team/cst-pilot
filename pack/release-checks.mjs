import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

export const sha256 = (data) => crypto.createHash('sha256').update(data).digest('hex');

// 运行态只能出现在测试副本；干净发行树出现这些路径就拒绝打包。
export function checkReleaseTree(root, files) {
  for (const rel of files) {
    if (/^pi\.exe$/i.test(rel)) {
      throw new Error(`pi.exe 不得位于发行根（应装配为 agent/.runtime/prx.bin）: ${rel}`);
    }
    if (/^agent\/home\/extensions\/web\/test\//i.test(rel)) {
      throw new Error(`Web 测试文件不得进入发行包: ${rel}`);
    }
    if (/(^|\/)(\.state|\.git|sessions|\.cache)(\/|$)|^agent\/home\/(auth\.json|models\.json|web-search\.json|fff\/|npm\/)|^wiztree\/(tmp\/|WizTree3\.ini(?:\.bad)?$)|(^|\/)\.env(?:\.|$)/i.test(rel)) {
      throw new Error(`发行树含运行状态或凭据路径: ${rel}`);
    }
    const buffer = fs.readFileSync(path.join(root, rel));
    if (/\b(?:sk-(?:proj-|ant-api\d+-)?[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|AIza[0-9A-Za-z_-]{30,}|AKIA[0-9A-Z]{16})\b|\beyJ[A-Za-z0-9_-]{15,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\b|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/.test(buffer.toString('latin1'))) {
      throw new Error(`发行文件含疑似密钥，需人工检查（不回显值）: ${rel}`);
    }
    if (rel.endsWith('.json')) {
      const inspect = (value) => {
        if (!value || typeof value !== 'object') return;
        for (const [key, item] of Object.entries(value)) {
          if (/^(api_?key|access_?token|refresh_?token|token|secret|password|client_?secret|authorization)$/i.test(key) && typeof item === 'string' && item.trim()) {
            throw new Error(`发行 JSON 含非空凭据字段 ${key}: ${rel}`);
          }
          inspect(item);
        }
      };
      inspect(JSON.parse(buffer.toString('utf8')));
    }
  }
}

// Web 静态产物与随包许可证的固定清单。缺失即拒绝打包，避免发行包静默少文件。
const WEB_STATIC = 'agent/home/extensions/web/static';
export const REQUIRED_RELEASE_FILES = [
  'LICENSE',
  'THIRD-PARTY-NOTICES.md',
  'licenses/pi-LICENSE.txt',
  'licenses/MPL-2.0.txt',
  'pwsh/LICENSE.txt',
  'wiztree/license.txt',
  `${WEB_STATIC}/index.html`,
  `${WEB_STATIC}/fonts/LICENSE.txt`,
  `${WEB_STATIC}/fonts/SourceHanSansCN-Regular.otf`,
  `${WEB_STATIC}/fonts/SourceHanSansCN-Medium.otf`,
  `${WEB_STATIC}/fonts/SourceHanSansCN-Bold.otf`,
  'agent/home/packages/pi-open-tui/LICENSE',
  'agent/home/packages/pi-web-access/LICENSE',
];

// 入口页与样式表引用的本地资源都必须落在发行树内。只检查带扩展名的绝对路径，
// 免得把路由地址当成文件。
const localRefs = (text) =>
  new Set(
    [...text.matchAll(/["'(](\/[A-Za-z0-9._/-]+\.(?:js|mjs|css|woff2?|otf|ttf|png|svg|jpg|webp|json))["')]/g)].map(
      (match) => match[1],
    ),
  );

export function checkReleaseContent(root, files) {
  const present = new Set(files);
  for (const rel of REQUIRED_RELEASE_FILES) {
    if (!present.has(rel)) throw new Error(`发行包缺少必需文件: ${rel}`);
  }
  const read = (rel) => fs.readFileSync(path.join(root, rel), 'utf8');
  const assertRefs = (rel, refs) => {
    for (const ref of refs) {
      if (!present.has(`${WEB_STATIC}${ref}`)) throw new Error(`${rel} 引用的资源不在发行树中: ${ref}`);
    }
  };
  const entry = `${WEB_STATIC}/index.html`;
  const entryRefs = localRefs(read(entry));
  if (![...entryRefs].some((ref) => ref.startsWith('/assets/'))) throw new Error('Web 入口页没有引用构建产物');
  assertRefs(entry, entryRefs);
  const styles = files.filter((rel) => rel.startsWith(`${WEB_STATIC}/assets/`) && rel.endsWith('.css'));
  if (styles.length === 0) throw new Error('Web 静态产物缺少样式表');
  for (const rel of styles) assertRefs(rel, localRefs(read(rel)));
}

export function verifyManifest(root, files) {
  const expected = new Map(fs.readFileSync(path.join(root, 'SHA256SUMS'), 'utf8').trim().split(/\r?\n/).map((line) => {
    const match = line.match(/^([a-f0-9]{64})  (.+)$/);
    if (!match) throw new Error('校验清单格式错误');
    return [match[2], match[1]];
  }));
  for (const rel of files) {
    if (rel === 'SHA256SUMS') continue;
    if (expected.get(rel) !== sha256(fs.readFileSync(path.join(root, rel)))) throw new Error(`校验失败或未列入清单: ${rel}`);
    expected.delete(rel);
  }
  if (expected.size) throw new Error(`清单文件缺失: ${[...expected.keys()].join(', ')}`);
}
