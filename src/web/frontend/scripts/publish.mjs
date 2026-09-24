import { cp, mkdir, readdir, rm } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const frontend = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = resolve(frontend, "build/client");
const target = resolve(frontend, "../../../agent/home/extensions/web/static");
const fonts = resolve(frontend, "../design/fonts");

await rm(target, { recursive: true, force: true });
await cp(source, target, { recursive: true });
try {
  const files = (await readdir(fonts)).filter((name) => /^SourceHanSansCN-(Regular|Medium|Bold)\.otf$/.test(name));
  await mkdir(resolve(target, "fonts"), { recursive: true });
  for (const name of files) await cp(resolve(fonts, name), resolve(target, "fonts", name));
  if (files.length !== 3) console.warn("思源黑体文件未齐备，页面将回退到系统字体；发行前须补齐字体。");
} catch (error) {
  if (error.code !== "ENOENT") throw error;
  console.warn("未找到本地思源黑体；发行前须从 OFL 发布物重建字体。");
}
console.log(`Web 静态文件已写入 ${target}`);
