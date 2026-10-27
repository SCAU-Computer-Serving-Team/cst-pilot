import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, readdir } from 'node:fs/promises';
import { test } from 'node:test';

const directory = new URL('../public/fonts/', import.meta.url);
const manifest = JSON.parse(await readFile(new URL('subset.json', directory), 'utf8'));
const includes = (character) => manifest.ranges.some(([first, last]) => character.codePointAt(0) >= first && character.codePointAt(0) <= last);

test('分发 WOFF2、OFL 许可与校验清单，不含完整 OTF', async () => {
  assert.ok(manifest.faces.length > 0);
  for (const face of manifest.faces) {
    const bytes = await readFile(new URL(face.file, directory));
    assert.equal(bytes.subarray(0, 4).toString(), 'wOF2');
    assert.equal(bytes.length, face.bytes);
    assert.equal(createHash('sha256').update(bytes).digest('hex'), face.sha256);
  }
  assert.equal((await readdir(directory)).some(name => name.endsWith('.otf')), false);
  assert.match(await readFile(new URL('LICENSE.txt', directory), 'utf8'), /SIL OPEN FONT LICENSE/);
  const sourceBytes = manifest.faces.reduce((sum, face) => sum + face.sourceBytes, 0);
  const subsetBytes = manifest.faces.reduce((sum, face) => sum + face.bytes, 0);
  assert.ok(subsetBytes < sourceBytes, `${subsetBytes} / ${sourceBytes}`);
});

test('常用中文、诊断字与界面标记有覆盖', () => {
  for (const character of '计算机维护队磁盘网络蓝屏诊断授权龟驱动') assert.ok(includes(character), character);
});
