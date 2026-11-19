import { access, cp, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const frontend = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = resolve(frontend, "../../..");
const source = resolve(frontend, "build/client");
const target = resolve(repoRoot, "agent/home/extensions/web/static");
const sharedFonts = resolve(repoRoot, "assets/fonts");

for (const name of ["SourceHanSansCN-VF.otf", "LICENSE.txt"]) {
  await access(resolve(sharedFonts, name));
}
await rm(resolve(source, "fonts"), { recursive: true, force: true });
await rm(target, { recursive: true, force: true });
await cp(source, target, { recursive: true });
console.log(`Web 静态文件已写入 ${target}`);
