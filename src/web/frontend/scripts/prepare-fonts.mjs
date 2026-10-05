import { createHash } from "node:crypto";
import { readFile, mkdir, readdir, rm, writeFile } from "node:fs/promises";
import { saveAsset as save } from "./asset-file.mjs";
import { spawnSync } from "node:child_process";
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

await mkdir(source, { recursive: true });
await mkdir(publicFonts, { recursive: true });
for (const [name, checksum] of files) {
  const { file, bytes, downloaded } = await verifiedFile(name, checksum);
  // A failed download/checksum must not replace a previously verified file.
  if (downloaded) await save(file, bytes);
  if (name === "LICENSE.txt") await save(resolve(publicFonts, name), bytes);
}
// 界面与诊断标签纳入字符集；用户消息不用作构建输入。
async function collect(directory) {
  const chunks = [];
  for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0)) {
    const file = resolve(directory, entry.name);
    if (entry.isDirectory()) chunks.push(...await collect(file));
    else if (/\.(?:tsx?|css)$/.test(entry.name)) chunks.push(await readFile(file, "utf8"));
  }
  return chunks;
}
const text = [...await collect(resolve(frontend, "app")), ...await collect(resolve(frontend, "../../../agent/home/extensions/diagnostics"))].join("\n");
const corpus = [...new Set(text)].sort().join("");
const recipe = createHash("sha256").update(await readFile(resolve(frontend, "scripts/subset-fonts.py"))).update(await readFile(resolve(frontend, "scripts/font-requirements.txt"))).update(corpus).update(revision).digest("hex");
let cached = false;
try {
  const manifest = JSON.parse(await readFile(resolve(publicFonts, "subset.json"), "utf8"));
  cached = manifest.recipe === recipe && manifest.faces.length === 3;
  for (const face of manifest.faces) {
    const bytes = await readFile(resolve(publicFonts, face.file));
    cached &&= createHash("sha256").update(bytes).digest("hex") === face.sha256;
  }
} catch { /* 字符集、配方或产物改变时重新生成。 */ }
if (cached) {
  console.log("Web 子集字体与 OFL 许可已校验，字符集未变。");
} else {
const temporary = resolve(source, `subset-${process.pid}`);
await mkdir(temporary, { recursive: true });
try {
  const corpusPath = resolve(temporary, "corpus.txt");
  await writeFile(corpusPath, corpus);
  const result = spawnSync(process.env.CST_WEB_FONT_PYTHON || "python", [resolve(frontend, "scripts/subset-fonts.py"), source, temporary, corpusPath], { encoding: "utf8" });
  if (result.error || result.status !== 0) throw new Error(`字体子集化失败。请安装 Python 与 scripts/font-requirements.txt。\n${result.stderr || result.error || ""}`);
  const manifest = JSON.parse(await readFile(resolve(temporary, "subset.json"), "utf8"));
  manifest.recipe = recipe;
  manifest.sourceRevision = revision;
  await writeFile(resolve(temporary, "subset.json"), JSON.stringify(manifest, null, 2) + "\n");
  for (const name of ["CSTUISans-Regular.woff2", "CSTUISans-Medium.woff2", "CSTUISans-Bold.woff2", "subset.json"]) {
    await save(resolve(publicFonts, name), await readFile(resolve(temporary, name)));
  }
  // 完整字体只用于画布，不进入 Web 静态产物。
  for (const [name] of files) if (name.endsWith(".otf")) await rm(resolve(publicFonts, name), { force: true });
  console.log(`Web 子集字体与 OFL 许可已准备就绪。${result.stdout.trim()}`);
} finally {
  await rm(temporary, { recursive: true, force: true });
}
}
await save(resolve(publicFonts, "NOTICE.txt"), "CST UI Sans is a character subset of Adobe Source Han Sans CN, distributed under the SIL Open Font License 1.1. The modified font uses a new family name; glyph outlines are unchanged. Original copyright and license are retained in LICENSE.txt.\n");
