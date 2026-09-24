import { access, cp, rm } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const frontend = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = resolve(frontend, "build/client");
const target = resolve(frontend, "../../../agent/home/extensions/web/static");
for (const name of ["SourceHanSansCN-Regular.otf", "SourceHanSansCN-Medium.otf", "SourceHanSansCN-Bold.otf", "LICENSE.txt"]) {
  await access(resolve(source, "fonts", name));
}
await rm(target, { recursive: true, force: true });
await cp(source, target, { recursive: true });
console.log(`Web 静态文件已写入 ${target}`);
