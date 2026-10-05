import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rename, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { saveAsset } from './asset-file.mjs';

const base = process.env.CST_WEB_TEST_TMPDIR ? join(process.env.CST_WEB_TEST_TMPDIR, new Date().toISOString().slice(0, 10)) : tmpdir();
await mkdir(base, { recursive: true });

test('字体内容不变时不替换文件，避免运行中的文件占用', async () => {
  const dir = await mkdtemp(join(base, 'font-write-'));
  try {
    const file = join(dir, 'font.woff2');
    await writeFile(file, 'same');
    await saveAsset(file, 'same', () => { throw new Error('不应替换'); });
    assert.equal((await readFile(file)).toString(), 'same');
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('短暂 EPERM 可恢复，永久占用保留原文件并清理暂存', async () => {
  const dir = await mkdtemp(join(base, 'font-write-'));
  const busy = Object.assign(new Error('busy'), { code: 'EPERM' });
  try {
    const file = join(dir, 'font.woff2');
    await writeFile(file, 'old');
    let attempts = 0;
    await saveAsset(file, 'new', async (from, to) => {
      if (++attempts < 3) throw busy;
      await rename(from, to);
    });
    assert.equal(attempts, 3);
    assert.equal((await readFile(file)).toString(), 'new');
    await assert.rejects(saveAsset(file, 'replacement', () => { throw busy; }), /busy/);
    assert.equal((await readFile(file)).toString(), 'new');
    assert.deepEqual(await readdir(dir), ['font.woff2']);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
