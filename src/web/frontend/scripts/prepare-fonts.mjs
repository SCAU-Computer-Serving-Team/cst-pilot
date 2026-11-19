import { createHash } from "node:crypto";
import { mkdir, readFile, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { saveAsset as save } from "./asset-file.mjs";

const frontend = resolve(fileURLToPath(new URL("..", import.meta.url)));
const source = resolve(frontend, "../../../design/fonts");
const sharedFonts = resolve(frontend, "../../../assets/fonts");
const legacyPublicFonts = resolve(frontend, "public/fonts");
const revision = "a4f7cf94edfb9d7ffbdfc4841de276358bd7e0f2";
const upstream = `https://raw.githubusercontent.com/adobe-fonts/source-han-sans/${revision}`;
const files = [
  [
    "SourceHanSansCN-VF.otf",
    "Variable/OTF/Subset/SourceHanSansCN-VF.otf",
    "AD10F878EA1AF97D66EC8BD10EB39798784F3A1EE3D5987A570EF6083F941EF3",
  ],
  [
    "LICENSE.txt",
    "LICENSE.txt",
    "FCAC737E761EC63DBFBDCE11030A1780161920D80315EDBA9C8BEFF1C2BAC5A2",
  ],
];

const sha256Upper = (bytes) => createHash("sha256").update(bytes).digest("hex").toUpperCase();

async function readVerifiedIfPresent(file, expected) {
  try {
    const bytes = await readFile(file);
    return sha256Upper(bytes) === expected ? bytes : null;
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
}

await rm(legacyPublicFonts, { recursive: true, force: true });
await mkdir(sharedFonts, { recursive: true });

let updated = false;
for (const [name, remotePath, expected] of files) {
  const target = resolve(sharedFonts, name);
  if (await readVerifiedIfPresent(target, expected)) continue;

  const cached = resolve(source, name);
  let bytes;
  let downloaded = false;
  try {
    bytes = await readFile(cached);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    if (process.env.CST_WEB_FONTS_OFFLINE === "1") {
      throw new Error(`缺少 ${name}；请从 Adobe Source Han Sans ${revision} 恢复到 ${source}，或联网重新执行。`);
    }
    let response;
    try {
      response = await fetch(`${upstream}/${remotePath}`, { signal: AbortSignal.timeout(120_000) });
    } catch (cause) {
      throw new Error(`无法下载 ${name}；请将官方文件放入 ${source} 后重试。`, { cause });
    }
    if (!response.ok) throw new Error(`下载 ${name} 失败：HTTP ${response.status}`);
    bytes = Buffer.from(await response.arrayBuffer());
    downloaded = true;
  }

  const actual = sha256Upper(bytes);
  if (actual !== expected) {
    throw new Error(`${name} 校验失败：需要 SHA-256 ${expected}，实际为 ${actual}`);
  }
  if (downloaded) {
    await mkdir(source, { recursive: true });
    await save(cached, bytes);
  }
  await save(target, bytes);
  updated = true;
}

console.log(updated ? "共享可变字体与 OFL 许可已同步至 assets/fonts。" : "共享可变字体与 OFL 许可已校验就绪。");
