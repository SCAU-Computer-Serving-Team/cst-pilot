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
  ...['doc/web/README.md', 'doc/web/MVP.md', 'doc/design/README.md', 'doc/design/web/tool/mapping.md', 'doc/Todo.md', 'doc/issues.md', 'DESIGN.md', 'CONTRIBUTING.md'].map(name => resolve(root, name)),
];
const slug = text => text.toLowerCase().replace(/`/g, '').replace(/[^\p{L}\p{N}_\- ]/gu, '').replace(/ /g, '-');

test('Web 规格与设计的本地 Markdown 链接及锚点可用', async () => {
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

test('SPEC 保留规则，阶段状态与实现职责分别集中管理', async () => {
  for (const name of await readdir(spec)) {
    if (!name.endsWith('.md')) continue;
    const content = await readFile(resolve(spec, name), 'utf8');
    assert.equal(/^状态：/m.test(content), false, name);
    assert.equal(/不是[^\n]*而是/.test(content), false, name);
    assert.equal(/^## (前端处理|目录划分|流式渲染策略|取值)$/m.test(content), false, name);
  }
});
