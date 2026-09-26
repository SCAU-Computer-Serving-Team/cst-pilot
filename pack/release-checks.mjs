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
