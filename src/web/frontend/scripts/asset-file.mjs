import { readFile, rename, rm, writeFile } from 'node:fs/promises';

/** 相同文件不重写；Windows 短暂占用时重试，失败保留原文件。 */
export async function saveAsset(file, bytes, replace = rename) {
  try {
    if ((await readFile(file)).equals(Buffer.from(bytes))) return;
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  const temporary = `${file}.${process.pid}.tmp`;
  try {
    await writeFile(temporary, bytes);
    for (let attempt = 0; ; attempt++) {
      try {
        await replace(temporary, file);
        break;
      } catch (error) {
        if (attempt >= 5 || !['EPERM', 'EACCES', 'EBUSY'].includes(error.code)) throw error;
        await new Promise(resolve => setTimeout(resolve, 50 * 2 ** attempt));
      }
    }
  } finally {
    await rm(temporary, { force: true });
  }
}
