import assert from 'node:assert/strict';
import { readFile, readdir, stat } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

async function collectMarkdown(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const results = [];
  for (const entry of entries) {
    const full = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      results.push(...(await collectMarkdown(full)));
    } else if (entry.isFile() && entry.name.endsWith('.md')) {
      results.push(full);
    }
  }
  return results;
}

const files = [
  ...(await collectMarkdown(resolve(root, 'doc'))),
  ...(await collectMarkdown(resolve(root, 'src/web/design/asset'))),
  ...['README.md', 'CHANGELOG.md', 'DESIGN.md', 'CONTRIBUTING.md', 'AGENTS.md', 'pack/README.md', 'lhm/README.md'].map(name => resolve(root, name)),
];
const slug = text => text.toLowerCase().replace(/`/g, '').replace(/[^\p{L}\p{N}_\- ]/gu, '').replace(/ /g, '-');

test('全项目文档的本地 Markdown 链接及锚点可用', async () => {
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
