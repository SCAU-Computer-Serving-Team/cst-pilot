import { createHash } from "node:crypto";
import { readFile, mkdir, rename, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const frontend = resolve(fileURLToPath(new URL("..", import.meta.url)));
const source = resolve(frontend, "../design/fonts");
const publicFonts = resolve(frontend, "public/fonts");
const revision = "a4f7cf94edfb9d7ffbdfc4841de276358bd7e0f2";
const upstream = `https://raw.githubusercontent.com/adobe-fonts/source-han-sans/${revision}`;
const files = [
  ["SourceHanSansCN-Regular.otf", "E2BC8A2E7F37474B774FFF8DB758681ECE40BB6947A90D571BCE9DD60671A8E4"],
  ["SourceHanSansCN-Medium.otf", "A94E558A2FE972BEE4F46BCE0843ABFF37063FD68C33F1E7D9058F6F09432B01"],
  ["SourceHanSansCN-Bold.otf", "62383707C086A32F3AFD5E293F34C7EFF64C7FEA31F579FDC6CBE34D920519A6"],
  ["LICENSE.txt", "FCAC737E761EC63DBFBDCE11030A1780161920D80315EDBA9C8BEFF1C2BAC5A2"],
];

async function verifiedFile(name, expected) {
  const file = resolve(source, name);
  let bytes;
  let downloaded = false;
  try {
    bytes = await readFile(file);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    if (process.env.CST_WEB_FONTS_OFFLINE === "1") {
      throw new Error(`缺少 ${name}；请从 Adobe Source Han Sans ${revision} 恢复到 ${source}，或联网重新执行。`);
    }
    const path = name === "LICENSE.txt" ? name : `SubsetOTF/CN/${name}`;
    let response;
    try {
      response = await fetch(`${upstream}/${path}`, { signal: AbortSignal.timeout(120_000) });
    } catch (cause) {
      throw new Error(`无法下载 ${name}；请将官方文件放入 ${source} 后重试。`, { cause });
    }
    if (!response.ok) throw new Error(`下载 ${name} 失败：HTTP ${response.status}`);
    bytes = Buffer.from(await response.arrayBuffer());
    downloaded = true;
  }
  const actual = createHash("sha256").update(bytes).digest("hex").toUpperCase();
  if (actual !== expected) throw new Error(`${name} 校验失败：需要 SHA-256 ${expected}，实际为 ${actual}`);
  return { file, bytes, downloaded };
}

async function save(file, bytes) {
  const temporary = `${file}.${process.pid}.tmp`;
  try {
    await writeFile(temporary, bytes);
    await rename(temporary, file);
  } finally {
    await rm(temporary, { force: true });
  }
}

await mkdir(source, { recursive: true });
await mkdir(publicFonts, { recursive: true });
for (const [name, checksum] of files) {
  const { file, bytes, downloaded } = await verifiedFile(name, checksum);
  // A failed download/checksum must not replace a previously verified file.
  if (downloaded) await save(file, bytes);
  await save(resolve(publicFonts, name), bytes);
}
console.log("Web 字体和 OFL 许可文件已校验并准备就绪。");
