// 颜色常量子集测试
// 扫描 cst-pilot-web.pen 中出现的全部颜色常量，断言每个都满足以下二者之一：
//   1. 在 cst-pilot-colors.pen 中定义（12 步色阶、语义色、首页 Blue hour 色表等）；
//   2. 在下方 EXCEPTIONS 清单中，且带有出处（DESIGN.md 例外章节等）。
// 新增颜色时：优先取色阶画布已定义的步；确需新色时在 EXCEPTIONS 登记出处。
// 运行：node --test src/test/pen-color-subset.test.mjs

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const webPenPath = path.join(root, "src/web/design/cst-pilot-web.pen");
const colorsPenPath = path.join(root, "src/web/design/cst-pilot-colors.pen");

/** 提取文本中的十六进制颜色。8 位含透明通道：全透明（alpha=00）视为不可见，跳过。 */
function extractColors(text) {
  const found = [];
  const re = /#([0-9a-fA-F]{6,8})\b/g;
  let m;
  while ((m = re.exec(text)) !== null) {
    const hex = m[1].toUpperCase();
    if (hex.length === 8) {
      if (hex.slice(6) === "00") continue; // 全透明，不产生视觉颜色
      found.push(hex.slice(0, 6));
    } else {
      found.push(hex);
    }
  }
  return found;
}

function count(colors) {
  const map = new Map();
  for (const c of colors) map.set(c, (map.get(c) ?? 0) + 1);
  return map;
}

/**
 * 例外清单：key = 大写 hex（不带 #），value = 出处。
 * 只收录「不在色阶画布中、但被设计文档明文允许」的颜色。
 */
const EXCEPTIONS = {
  // DESIGN.md「聊天页浅色例外」：聊天页用无色偏灰白，不取 12 步中性色阶
  "202020": "DESIGN.md 聊天页浅色例外 · 正文",
  "646464": "DESIGN.md 聊天页浅色例外 · 次要文字（卡片文字统一降一档）",
  "F6F6F6": "DESIGN.md 聊天页浅色例外 · 页面底色/原文块灰底",
  "F0F0F0": "DESIGN.md 聊天页浅色例外 · 弱边框/调用语句片",
  "E5E5E5": "DESIGN.md 聊天页浅色例外 · 边框",
  "EAEAEA": "DESIGN.md 聊天页浅色例外 · 悬停/选中底色",
  "D5D5D5": "登录卡片输入框描边（聊天页例外灰体系内）",
  "F5F6F8": "主页侧栏底色 light-sidebar（DESIGN.md colors）",
  // DESIGN.md colors 记录的产品色，色阶画布未收录
  "465A9F": "DESIGN.md light-send · 发送/登录/进度弧强调色（Blue hour 深段）",
  "161C4A": "登录深色 AIR 底色（asset/blue-hour-air-dark-palette.png 色表）",
  // 既有帧 context 记录的深色体系取值
  "1B1C22": "深色悬停面板弱边框/分隔线（上下文面板·悬停等帧 context）",
  "111733": "深色面板投影 shadow #11173333",
  "E0E0E0": "聊天工作台 HeroUI 表格行描边（主画布既有元件）",
};

test("cst-pilot-web.pen 的颜色常量是色阶画布定义色与登记例外的子集", () => {
  const palette = new Set(extractColors(readFileSync(colorsPenPath, "utf8")));
  const used = count(extractColors(readFileSync(webPenPath, "utf8")));

  const unknown = [...used.entries()]
    .filter(([c]) => !palette.has(c) && !(c in EXCEPTIONS))
    .sort((a, b) => b[1] - a[1]);

  const lines = unknown.map(
    ([c, n]) => `  #${c} ×${n}（色阶画布无此色，EXCEPTIONS 无登记）`,
  );
  assert.equal(
    unknown.length,
    0,
    `发现 ${unknown.length} 个未登记颜色：\n${lines.join("\n")}\n` +
      `处理：取色阶画布已有步替换，或在 EXCEPTIONS 中登记出处。`,
  );
});

test("例外清单无冗余（登记的颜色不应已在色阶画布中定义）", () => {
  const palette = new Set(extractColors(readFileSync(colorsPenPath, "utf8")));
  const redundant = Object.keys(EXCEPTIONS).filter((c) => palette.has(c));
  assert.deepEqual(
    redundant,
    [],
    `以下例外已在色阶画布定义，应从 EXCEPTIONS 移除：${redundant.join(" ")}`,
  );
});

test("例外清单中的颜色确实在 web 画布中被使用（防止登记幽灵条目）", () => {
  const used = new Set(extractColors(readFileSync(webPenPath, "utf8")));
  const ghosts = Object.keys(EXCEPTIONS).filter((c) => !used.has(c));
  assert.deepEqual(
    ghosts,
    [],
    `以下例外登记后未在画布使用，应移除：${ghosts.join(" ")}`,
  );
});
