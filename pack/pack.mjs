#!/usr/bin/env node
// ============================================================
// pack.mjs — cst-pilot 0.4 发行构建脚本
//
// 官方 pi.exe + 本仓库内容。测试使用副本，ZIP 按清单生成。
//
// 用法（在仓库根，用仓库自带 node 运行）：
//   node\node.exe pack\pack.mjs --official <官方zip或解压目录> --out <新目录> [--zip]
//
//   --official  官方 pi-windows-x64-<ver>.zip 或其解压目录（含 pi.exe）
//   --out       构建输出目录（新建 cst-pilot/；已存在且非空则拒绝，防覆盖）
//   --zip       装配完成后额外生成发行 zip
//
// 首次运行需联网下载 esbuild（npm 缓存），之后离线可复现。
// ============================================================

import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { checkReleaseTree, sha256, verifyManifest } from "./release-checks.mjs";
import { smokeRelease } from "./smoke.mjs";

// ---------- 构建配置（发行工程单一事实源） ----------

const CONFIG = {
  VERSION: "0.4.0",
  PI_VERSION: "0.85.1",
  PI_EXE_SHA256: "2d4d351da30bfe23a473032e66a571b238763565aa93754e74f4a939de13f195",
  ESBUILD_VERSION: "0.25.10",
  // 扩展真实版本（pi-fff 曾错标 0.1.0，修正为上游真实版本）
  EXT_VERSIONS: { "pi-fff": "0.10.6", "pi-web-access": "0.18.0", "pi-open-tui": "0.2.10" },

  // esbuild 打包的两个扩展：npm 包（源码形态）→ 预打包产物
  BUNDLE: [
    {
      name: "pi-web-access",
      entry: "agent/home/npm/node_modules/pi-web-access/index.ts",
      externals: ["@earendil-works/*", "node:*"],
    },
    {
      name: "pi-fff",
      entry: "agent/home/npm/node_modules/@ff-labs/pi-fff/src/index.ts",
      externals: ["@earendil-works/*", "node:*", "@ff-labs/*", "@sinclair/typebox"],
    },
  ],

  // pi-fff 的原生依赖链（从 npm 安装目录原样复制到 packages/pi-fff/node_modules）
  FFF_NATIVE_PACKAGES: [
    "ffi-rs",
    "@yuuang/ffi-rs-win32-x64-msvc",
    "@ff-labs/fff-bin-win32-x64",
    "@ff-labs/fff-bun",
    "@ff-labs/fff-node",
  ],

  // 官方运行资源；保留图片资源，避免内置界面缺图。
  OFFICIAL_ITEMS: [
    { p: "pi.exe", f: true },
    { p: "package.json", f: true },
    { p: "photon_rs_bg.wasm", f: true },
    { p: "theme", d: true },
    { p: "export-html", d: true },
    { p: "native", d: true },
    { p: "node_modules", d: true },
    { p: "assets", d: true },
  ],

  // 仓库 → 发行根
  REPO_ROOT_ITEMS: [
    { p: "pi.cmd", f: true },
    { p: "README.md", f: true },
    { p: "AGENTS.md", f: true },
    { p: "biome.json", f: true },
    { p: "THIRD-PARTY-NOTICES.md", f: true },
  ],

  // 仓库 → 发行树（带排除过滤）
  REPO_FILTERED: [
    { src: "assets", dst: "assets", exclude: ["make-logo.cjs"] }, // 只发行图片，不带生成脚本
    { src: "doc", dst: "doc", exclude: ["test/"] },
    { src: "pwsh", dst: "pwsh", exclude: ["Schemas/", "preview/", "Install-PowerShellRemoting.ps1", "RegisterManifest.ps1"] },
    { src: "wiztree", dst: "wiztree", exclude: ["WizTree3.ini", "WizTree3.ini.bad", "tmp/"] }, // 运行态配置与备份不进发行包
    { src: "lhm", dst: "lhm", exclude: [] },
    { src: "agent/home/bin", dst: "agent/home/bin", exclude: [] },
    { src: "agent/home/skills", dst: "agent/home/skills", exclude: [] },
    { src: "agent/home/extensions", dst: "agent/home/extensions", exclude: ["web/test/"] },
  ],

  // agent/home 散文件（发行版白名单；排除运行态与开发态：npm/、sessions/、fff/、
  // auth.json、models.json、web-search.json）。**API key 不随包分发**：队员首跑在
  // pi 内 /login 填写网关凭据（写入发行树的 agent/home/auth.json，属该机的本地状态）；
  // models-store.json 仅模型目录缓存，无凭据，随包保留（离线可用模型列表）。
  REPO_HOME_FILES: ["APPEND_SYSTEM.md", "models-store.json", "open-tui.json"],

  // 发行 settings.json 生成（packages 用本地路径，不触发任何安装）
  RELEASE_SETTINGS: {
    _comment:
      "默认开放读取、检索和诊断工具。扩展随包提供，启动不安装或更新。配置与会话保存在 agent/home，临时状态保存在 .state。此配置不是系统沙箱；PowerShell 等原生组件可能留下宿主缓存。",
    defaultProvider: "opencode-go",
    defaultModel: "glm-5.3-flash",
    defaultTools: ["read", "ls"],
    defaultProjectTrust: "never",
    enableInstallTelemetry: false,
    lastChangelogVersion: "0.85.1",
    theme: "dark",
    packages: ["./packages/pi-fff", "./packages/pi-open-tui", "./packages/pi-web-access"],
  },
};

// ---------- 工具函数 ----------

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
let step = 0;
function banner(msg) {
  step += 1;
  console.log(`\n[${step}] ${msg}`);
}
function die(msg) {
  console.error(`\n[pack] 失败: ${msg}`);
  process.exit(1);
}
function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: "inherit", ...opts });
  if (r.status !== 0) die(`${cmd} 退出码 ${r.status}${r.error ? ` (${r.error.message})` : ""}`);
}
function copyFiltered(src, dst, exclude) {
  fs.cpSync(src, dst, {
    recursive: true,
    filter: (s) => {
      const rel = path.relative(src, s).replaceAll("\\", "/");
      if (rel === "") return true;
      return !exclude.some((x) => (x.endsWith("/") ? rel === x.slice(0, -1) || rel.startsWith(x) : rel === x));
    },
  });
}
function walk(dir, base = dir, skip = []) {
  const out = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (skip.includes(e.name)) continue;
    const p = path.join(dir, e.name);
    if (e.isSymbolicLink()) die(`不允许将符号链接或目录联接打入发行包: ${p}`);
    if (e.isDirectory()) out.push(...walk(p, base, skip));
    else if (e.isFile()) out.push(path.relative(base, p).replaceAll("\\", "/"));
    else die(`不支持的发行文件类型: ${p}`);
  }
  return out.sort();
}

// ---------- 参数 ----------

const argv = process.argv.slice(2);
function argOf(name) {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
}
const official = argOf("--official");
const outRoot = argOf("--out");
const esbuildPath = argOf("--esbuild");
const wantZip = argv.includes("--zip");
if (!official || !outRoot) {
  console.error("用法: node\\node.exe pack\\pack.mjs --official <官方zip或目录> --out <新目录> [--zip]");
  process.exit(1);
}

const buildRoot = path.resolve(outRoot);
if (fs.existsSync(buildRoot) && fs.readdirSync(buildRoot).length > 0) die(`输出目录非空，拒绝覆盖: ${buildRoot}`);
const out = path.join(buildRoot, "cst-pilot");
fs.mkdirSync(out, { recursive: true });

// ---------- [1] 准备官方 pi 包 ----------

banner(`准备官方 pi 包（目标 ${CONFIG.PI_VERSION}）`);
let officialDir;
const officialFiles = JSON.parse(fs.readFileSync(path.join(scriptDir, "official-windows-x64.json"), "utf8"));
const st = fs.statSync(official);
if (st.isDirectory()) {
  officialDir = path.resolve(official);
  if (!fs.existsSync(path.join(officialDir, "pi.exe"))) die(`目录中无 pi.exe: ${officialDir}`);
  console.log(`使用已解压目录: ${officialDir}`);
} else {
  if (sha256(fs.readFileSync(official)) !== officialFiles.archiveSha256) die("官方 ZIP 校验失败");
  officialDir = path.join(buildRoot, "_official");
  fs.mkdirSync(officialDir, { recursive: true });
  console.log(`解压官方 zip...`);
  run("tar.exe", ["-xmf", path.resolve(official), "-C", officialDir]);
  if (!fs.existsSync(path.join(officialDir, "pi.exe"))) die(`zip 解压后无 pi.exe: ${officialDir}`);
}
const officialVersion = JSON.parse(fs.readFileSync(path.join(officialDir, "package.json"), "utf8")).version;
if (officialVersion !== CONFIG.PI_VERSION) {
  die(`官方包版本 ${officialVersion} ≠ 配置版本 ${CONFIG.PI_VERSION}；请更新 pack.mjs 的 PI_VERSION 并走全量验收`);
}
if (sha256(fs.readFileSync(path.join(officialDir, "pi.exe"))) !== CONFIG.PI_EXE_SHA256) die("pi.exe 与已核验的官方 0.85.1 二进制不一致");
const officialPaths = CONFIG.OFFICIAL_ITEMS.flatMap((item) => item.f ? [item.p] : walk(path.join(officialDir, item.p)).map((p) => `${item.p}/${p}`));
if (officialPaths.length !== Object.keys(officialFiles.files).length) die("官方资源文件数不符");
for (const p of officialPaths) {
  if (sha256(fs.readFileSync(path.join(officialDir, p))) !== officialFiles.files[p]) die(`官方资源校验失败: ${p}`);
}

const sourceLockPath = path.join(scriptDir, "extensions.package-lock.json");
const sourceLock = JSON.parse(fs.readFileSync(sourceLockPath, "utf8"));
const installedPackages = {};
for (const [relative, locked] of Object.entries(sourceLock.packages)) {
  if (!relative || !locked.version) continue;
  const manifest = path.join(repoRoot, "agent/home/npm", relative, "package.json");
  if (!fs.existsSync(manifest)) continue; // 其他平台的可选依赖未安装。
  const actual = JSON.parse(fs.readFileSync(manifest, "utf8"));
  if (actual.version !== locked.version) die(`已安装依赖与锁文件不符: ${relative}`);
  installedPackages[relative] = { version: actual.version, integrity: locked.integrity };
}
const localLock = JSON.parse(fs.readFileSync(path.join(repoRoot, "agent/home/npm/package-lock.json"), "utf8"));
if (JSON.stringify(localLock.packages) !== JSON.stringify(sourceLock.packages)) die("扩展锁文件已变化，请先审查并更新 pack/extensions.package-lock.json");
for (const [name, version] of Object.entries(CONFIG.EXT_VERSIONS)) {
  const npmName = name === "pi-fff" ? "@ff-labs/pi-fff" : name;
  if (installedPackages[`node_modules/${npmName}`]?.version !== version) die(`扩展版本不符: ${npmName}，需要 ${version}`);
}

// ---------- [2] esbuild 预打包扩展 ----------

banner("esbuild 预打包扩展（web-access、fff）");
const npxCli = path.join(repoRoot, "node", "node_modules", "npm", "bin", "npx-cli.js");
if (esbuildPath) {
  const version = spawnSync(process.execPath, [path.resolve(esbuildPath), "--version"], { encoding: "utf8" });
  if (version.status !== 0 || version.stdout.trim() !== CONFIG.ESBUILD_VERSION) die(`需要 esbuild ${CONFIG.ESBUILD_VERSION}`);
} else if (!fs.existsSync(npxCli)) die(`未找到 npm 入口: ${npxCli}`);
const packagesDir = path.join(out, "agent", "home", "packages");
fs.mkdirSync(packagesDir, { recursive: true });
for (const b of CONFIG.BUNDLE) {
  const entry = path.join(repoRoot, b.entry);
  if (!fs.existsSync(entry)) die(`扩展入口不存在: ${entry}`);
  const outdir = path.join(packagesDir, b.name);
  fs.mkdirSync(outdir, { recursive: true });
  const externals = b.externals.flatMap((e) => ["--external:" + e]);
  run(process.execPath, [
    ...(esbuildPath ? [path.resolve(esbuildPath)] : [npxCli, "-y", `esbuild@${CONFIG.ESBUILD_VERSION}`]),
    "--bundle", entry, "--format=esm", "--platform=node", "--target=node20",
    "--splitting", `--outdir=${outdir}`, ...externals,
  ]);
  console.log(`  ${b.name}: ${fs.readdirSync(outdir).length} 个产物文件`);
  const sourcePackage = path.dirname(entry);
  const packageRoot = b.name === "pi-fff" ? path.dirname(sourcePackage) : sourcePackage;
  for (const f of fs.readdirSync(packageRoot).filter((f) => /^(LICENSE|COPYING|NOTICE)(\.|$)/i.test(f))) {
    if (fs.statSync(path.join(packageRoot, f)).isFile()) fs.copyFileSync(path.join(packageRoot, f), path.join(outdir, f));
  }
}

// pi-fff 原生依赖链（.node 不可内联，原样复制）
const fffNm = path.join(packagesDir, "pi-fff", "node_modules");
fs.mkdirSync(fffNm, { recursive: true });
for (const p of CONFIG.FFF_NATIVE_PACKAGES) {
  const src = path.join(repoRoot, "agent", "home", "npm", "node_modules", p);
  if (!fs.existsSync(src)) die(`fff 依赖缺失（先在开发环境安装）: ${src}`);
  fs.cpSync(src, path.join(fffNm, p), { recursive: true });
}
console.log(`  pi-fff 原生链: ${CONFIG.FFF_NATIVE_PACKAGES.length} 个包`);

// pi-open-tui 零依赖，整包原样
fs.cpSync(
  path.join(repoRoot, "agent", "home", "npm", "node_modules", "pi-open-tui"),
  path.join(packagesDir, "pi-open-tui"),
  { recursive: true },
);
// 扩展包装 package.json（pi.extensions 指向 bundle 入口；版本用真实版本）
for (const name of ["pi-fff", "pi-web-access"]) {
  fs.writeFileSync(
    path.join(packagesDir, name, "package.json"),
    JSON.stringify({ name, version: CONFIG.EXT_VERSIONS[name], type: "module", pi: { extensions: ["./index.js"] } }),
  );
}

// ---------- [3] 白名单装配 ----------

banner("白名单装配（官方 + 仓库 + 生成）");
for (const it of CONFIG.OFFICIAL_ITEMS) {
  const src = path.join(officialDir, it.p);
  if (!fs.existsSync(src)) die(`官方包缺少 ${it.p}（官方布局变更？）`);
  // 官方资源全部随 pi.exe 藏入 agent/.runtime（SEA 按自身目录找 theme/assets 等）；
  // pi.exe 改名 prx.bin，防绕过启动器直跑裸 pi；内容哈希锁不变。
  const dst = path.join(out, "agent", ".runtime", it.p === "pi.exe" ? "prx.bin" : it.p);
  fs.mkdirSync(path.dirname(dst), { recursive: true });
  it.f ? fs.copyFileSync(src, dst) : fs.cpSync(src, dst, { recursive: true });
}
console.log("  官方运行资源已复制");

for (const it of CONFIG.REPO_ROOT_ITEMS) {
  const src = path.join(repoRoot, it.p);
  if (!fs.existsSync(src)) die(`仓库缺少 ${it.p}`);
  const dst = path.join(out, it.p);
  it.f ? fs.copyFileSync(src, dst) : fs.cpSync(src, dst, { recursive: true });
}
for (const it of CONFIG.REPO_FILTERED) {
  const src = path.join(repoRoot, it.src);
  if (!fs.existsSync(src)) die(`仓库缺少 ${it.src}`);
  copyFiltered(src, path.join(out, it.dst), it.exclude);
}
for (const f of CONFIG.REPO_HOME_FILES) {
  const src = path.join(repoRoot, "agent", "home", f);
  if (!fs.existsSync(src)) die(`仓库缺少 agent/home/${f}`);
  fs.copyFileSync(src, path.join(out, "agent", "home", f));
}
console.log("  仓库侧: pi.cmd/文档/pwsh/wiztree/lhm/home 资源");
fs.cpSync(path.join(scriptDir, "licenses"), path.join(out, "licenses"), { recursive: true });

// ---------- [4] 发行 settings.json ----------

banner("生成发行 settings.json（本地路径扩展 + 遥测关闭）");
fs.writeFileSync(path.join(out, "agent", "home", "settings.json"), JSON.stringify(CONFIG.RELEASE_SETTINGS, null, 2) + "\n");

fs.writeFileSync(path.join(out, "BUILD-INFO.json"), JSON.stringify({
  version: CONFIG.VERSION, piVersion: CONFIG.PI_VERSION, piExeSha256: CONFIG.PI_EXE_SHA256,
  esbuildVersion: CONFIG.ESBUILD_VERSION, extensionLockSha256: sha256(fs.readFileSync(sourceLockPath)), installedPackages,
}, null, 2) + "\n");

// ---------- [5] 检查干净发行树并生成清单 ----------

banner("生成 VERSION 与 SHA256SUMS");
fs.writeFileSync(path.join(out, "VERSION"), `cst-pilot ${CONFIG.VERSION}\npi ${CONFIG.PI_VERSION}\n`);
const files = walk(out);
checkReleaseTree(out, files);
const sums = files.map((rel) => {
  const h = crypto.createHash("sha256");
  h.update(fs.readFileSync(path.join(out, rel)));
  return `${h.digest("hex")}  ${rel}`;
});
fs.writeFileSync(path.join(out, "SHA256SUMS"), sums.join("\n") + "\n");
console.log(`  VERSION: cst-pilot ${CONFIG.VERSION} / pi ${CONFIG.PI_VERSION}`);
console.log(`  SHA256SUMS: ${files.length} 个文件`);

// ---------- [6] 打包后自动冒烟 ----------

banner("独立副本冒烟（本机模拟模型，不使用真实密钥）");
if (argv.includes("--skip-smoke")) {
  if (wantZip) die("正式 ZIP 不允许跳过冒烟；--skip-smoke 仅用于检查装配目录");
  console.log("  已跳过；此目录未通过启动验收");
} else {
  await smokeRelease(out, path.join(buildRoot, "_smoke"));
  console.log("  启动、工具注册和模拟模型回合通过");
}

// ---------- [7] 统计（排除冒烟产生的运行态） ----------

banner("复核发行树和校验清单");
const releaseFiles = walk(out);
checkReleaseTree(out, releaseFiles);
verifyManifest(out, releaseFiles);
let bytes = 0;
for (const rel of releaseFiles) bytes += fs.statSync(path.join(out, rel)).size;
console.log(`  发行树: ${releaseFiles.length} 个文件 / ${(bytes / 1024 / 1024).toFixed(1)} MB（不含运行态）`);

// ---------- [8] 可选 zip ----------

if (wantZip) {
  banner("生成发行 zip");
  const zipPath = path.join(buildRoot, `cst-pilot-${CONFIG.VERSION}.zip`);
  const listPath = path.join(buildRoot, "_zip-inputs.txt");
  const expected = releaseFiles.map((rel) => `cst-pilot/${rel}`);
  fs.writeFileSync(listPath, expected.join("\0") + "\0");
  run("tar.exe", ["-a", "-c", "-f", zipPath, "-C", buildRoot, "--null", "-T", listPath]);
  const listing = spawnSync("tar.exe", ["-tf", zipPath], { encoding: "utf8", maxBuffer: 4 * 1024 * 1024 });
  if (listing.status !== 0 || listing.stdout.trim().split(/\r?\n/).sort().join("\n") !== expected.sort().join("\n")) die("最终 ZIP 文件清单不一致");
  const verifyRoot = path.join(buildRoot, "_verify");
  fs.mkdirSync(verifyRoot);
  run("tar.exe", ["-xmf", zipPath, "-C", verifyRoot]);
  const extractedRoot = path.join(verifyRoot, "cst-pilot");
  verifyManifest(extractedRoot, walk(extractedRoot));
  fs.writeFileSync(path.join(buildRoot, "SHA256SUMS"), `${sha256(fs.readFileSync(zipPath))}  ${path.basename(zipPath)}\n`);
  fs.copyFileSync(path.join(out, "VERSION"), path.join(buildRoot, "VERSION"));
  const z = fs.statSync(zipPath);
  console.log(`  zip: ${(z.size / 1024 / 1024).toFixed(1)} MB → ${zipPath}`);
}

console.log(`\n[pack] 完成: ${out}`);
