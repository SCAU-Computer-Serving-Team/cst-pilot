// 生成 cst-pilot 的 12 步色阶，输出 DTCG 2025.10 令牌文件。
//
// 用法：node make-color-scale.mjs [输出路径]
// 默认输出同目录下的 cst-pilot-colors.tokens.json
//
// 色阶不是手挑的固定值，由 oklch 曲线算出：
//   明度  —— 12 步各有一个基准明度，浅深两套独立，不用反转或混色派生
//   色度  —— 每步一个相对系数，第 9 步（实心色块）拉满
//   黄色系 —— 第 9/10 步按色相提亮。黄色在低明度下必然是土黄，
//             参考 Radix 的 yellow-9（oklch(0.915 0.182 100.4)）得出这条偏移
//
// 超出 sRGB 色域的取值按「保明度与色相、压低色度」处理。

import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

const D = Math.PI / 180;

function oklabToLinearSrgb(L, a, b) {
  const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = L - 0.0894841775 * a - 1.291485548 * b;
  const l = l_ ** 3;
  const m = m_ ** 3;
  const s = s_ ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

function gamma(x) {
  return x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055;
}

function inGamut(L, C, H) {
  const [r, g, b] = oklabToLinearSrgb(L, C * Math.cos(H * D), C * Math.sin(H * D));
  const eps = 1e-6;
  return r >= -eps && r <= 1 + eps && g >= -eps && g <= 1 + eps && b >= -eps && b <= 1 + eps;
}

function maxChroma(L, H) {
  let lo = 0;
  let hi = 0.42;
  for (let i = 0; i < 44; i++) {
    const mid = (lo + hi) / 2;
    if (inGamut(L, mid, H)) lo = mid;
    else hi = mid;
  }
  return lo;
}

function resolve(L, C, H) {
  const c = Math.min(C, maxChroma(L, H));
  const [r, g, b] = oklabToLinearSrgb(L, c * Math.cos(H * D), c * Math.sin(H * D));
  const f = (x) => Math.max(0, Math.min(255, Math.round(gamma(x) * 255)));
  const hex = `#${[f(r), f(g), f(b)].map((v) => v.toString(16).padStart(2, "0")).join("")}`;
  return { colorSpace: "oklch", components: [+L.toFixed(4), +c.toFixed(4), +H.toFixed(1)], hex };
}

const LIGHT_L = [0.993, 0.981, 0.954, 0.937, 0.914, 0.895, 0.871, 0.845, 0.62, 0.585, 0.51, 0.24];
const DARK_L = [0.145, 0.175, 0.213, 0.255, 0.286, 0.32, 0.38, 0.45, 0.62, 0.66, 0.76, 0.93];
const CHROMA_FACTOR = [0.03, 0.06, 0.15, 0.2, 0.26, 0.31, 0.39, 0.5, 1.0, 0.96, 0.86, 0.58];
const BASE_L = { light: LIGHT_L, dark: DARK_L };

function yellowBoost(hue) {
  const t = Math.cos((hue - 100) * D);
  return t <= 0 ? 0 : 0.3 * t ** 6;
}

function colorScale(theme, hue, sat) {
  const base = BASE_L[theme];
  const boost = yellowBoost(hue);
  const peak = maxChroma(Math.min(0.97, base[8] + boost), hue) * sat;
  return base.map((l, i) => {
    const ll = i === 8 || i === 9 ? Math.min(0.97, l + boost) : l;
    return resolve(ll, peak * CHROMA_FACTOR[i], hue);
  });
}

function neutralScale(theme, hue, chroma) {
  return BASE_L[theme].map((l) => resolve(l, chroma, hue));
}

// 已采用的方案：中性偏蓝，彩色四系
const NEUTRAL = { hue: 230, chroma: 0.012 };
const SATURATION = 0.94;
const HUES = { accent: 254, success: 151, warning: 72, danger: 26 };

const color = {};
for (const theme of ["light", "dark"]) {
  color[theme] = { neutral: neutralScale(theme, NEUTRAL.hue, NEUTRAL.chroma) };
  for (const [name, hue] of Object.entries(HUES)) {
    color[theme][name] = colorScale(theme, hue, SATURATION);
  }
}

// 展开成 DTCG：每步一个令牌
const out = {
  $schema: "https://www.designtokens.org/schemas/2025.10/format.json",
  $description:
    "cst-pilot 自有配色。12 步色阶，每步绑定一种界面用途，取色依据是用途而不是明暗编号。浅深两套独立生成。详细划分见仓库根 DESIGN.md 的 Colors 一节。",
  color: { $type: "color" },
};

for (const [theme, families] of Object.entries(color)) {
  out.color[theme] = {};
  for (const [name, steps] of Object.entries(families)) {
    out.color[theme][name] = {};
    steps.forEach((value, i) => {
      out.color[theme][name][String(i + 1)] = { $value: value };
    });
  }
}

const here = path.dirname(fileURLToPath(import.meta.url));
const target = process.argv[2] ?? path.join(here, "cst-pilot-colors.tokens.json");
fs.writeFileSync(target, `${JSON.stringify(out, null, 2)}\n`);

console.log(`written: ${target}`);
for (const [theme, families] of Object.entries(color)) {
  console.log(`\n-- ${theme}`);
  for (const [name, steps] of Object.entries(families)) {
    console.log(`${name.padEnd(8)}${steps.map((s) => s.hex.padStart(8)).join("")}`);
  }
}
