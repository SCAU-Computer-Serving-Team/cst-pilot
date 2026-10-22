import assert from 'node:assert/strict';
import { readFile, readdir, stat } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const spec = resolve(root, 'doc/web/SPEC');
const design = resolve(root, 'doc/design/web');
const files = [
  ...await readdir(spec).then(names => names.filter(name => name.endsWith('.md')).map(name => resolve(spec, name))),
  ...await readdir(design).then(names => names.filter(name => name.endsWith('.md')).map(name => resolve(design, name))),
  ...(await Promise.all(['doc/telemetry', 'doc/telemetry/sender', 'doc/telemetry/receiver'].map(async directory =>
    (await readdir(resolve(root, directory))).filter(name => name.endsWith('.md')).map(name => resolve(root, directory, name))
  ))).flat(),
  ...['README.md', 'CHANGELOG.md', 'doc/PRD.md', 'doc/Notice.md', 'doc/contract.md', 'pack/README.md', 'doc/web/README.md', 'doc/web/MVP.md', 'doc/design/README.md', 'doc/design/release.md', 'doc/design/web/tool/mapping.md', 'doc/Todo.md', 'doc/issues.md', 'doc/auth/README.md', 'doc/auth/integration.md', 'doc/test/README.md', 'doc/test/account-report.md', 'doc/test/telemetry-report.md', 'doc/test/launcher-report.md', 'doc/test/checkpoint5-report.md', 'DESIGN.md', 'CONTRIBUTING.md'].map(name => resolve(root, name)),
];
const slug = text => text.toLowerCase().replace(/`/g, '').replace(/[^\p{L}\p{N}_\- ]/gu, '').replace(/ /g, '-');

test('Web、认证与验收文档的本地 Markdown 链接及锚点可用', async () => {
  const failures = [];
  for (const file of files) {
    const content = await readFile(file, 'utf8');
    for (const match of content.matchAll(/\[[^\]\n]+\]\(([^)\s]+)\)/g)) {
      const ref = match[1];
      if (/^(https?:|mailto:)/.test(ref)) continue;
      const [path, fragment] = ref.split('#');
      const target = path ? resolve(dirname(file), decodeURIComponent(path)) : file;
      try {
        const info = await stat(target);
        if (fragment && info.isFile() && target.endsWith('.md')) {
          const source = await readFile(target, 'utf8');
          const headings = [...source.matchAll(/^#{1,6} (.+)$/gm)].map(item => slug(item[1]));
          if (!headings.includes(decodeURIComponent(fragment))) failures.push(`${file}: ${ref} 锚点不存在`);
        }
      } catch { failures.push(`${file}: ${ref} 路径不存在`); }
    }
  }
  assert.deepEqual(failures, []);
});
