import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const web = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
const css = readFileSync(new URL("../web/frontend/app/styles.css", import.meta.url), "utf8");
const rules = readFileSync(new URL("../../DESIGN.md", import.meta.url), "utf8");
function walk(node, visit) {
  visit(node);
  for (const child of node.children ?? []) walk(child, visit);
}

test("主画布的节点 ID 唯一，工作帧没有遗留占位标记", () => {
  const ids = new Set();
  for (const root of web.children) walk(root, (node) => {
    assert.ok(!ids.has(node.id), `重复 ID：${node.id}`);
    ids.add(node.id);
    assert.notEqual(node.placeholder, true, `未完成工作帧：${node.name}`);
  });
});

test("四个模型服务配置帧使用 816 设置内容列，保持页面节点 ID", () => {
  for (const id of ["rdTGN", "n3jW1", "i1jzTi", "LfOD8"]) {
    const root = web.children.find((node) => node.id === id);
    assert.ok(root);
    const sections = [];
    walk(root, (node) => {
      if (node.name === "模型服务 · 设置内容列") sections.push(node);
      assert.notEqual(node.name, "共享模型服务表单", "不能回到登录卡片布局");
    });
    assert.equal(sections.length, 1);
    assert.equal(sections[0].width, 816);
    const card = sections[0].children.find((node) => node.name === "模型服务 · 设置卡片");
    assert.equal(card.cornerRadius, 12);
  }
});

test("画布导航覆盖九个主题，浅深配置帧成对排列", () => {
  assert.ok(web.children.some((node) => node.name === "00 · 画布导航"));
  assert.equal(web.children.filter((node) => node.name?.includes("分区标题")).length, 9);
  for (const [lightId, darkId] of [["rdTGN", "n3jW1"], ["i1jzTi", "LfOD8"]]) {
    const light = web.children.find((node) => node.id === lightId);
    const dark = web.children.find((node) => node.id === darkId);
    assert.equal(light.y, dark.y);
    assert.equal(dark.x - light.x, 2200);
  }
});

test("工具与表格遵循页面骨架，字段共用数值列", () => {
  assert.ok(css.includes(".tool-group, .tool-card { width: 100%; margin-left: 0; }"));
  const regions = [];
  for (const root of web.children) walk(root, (node) => { if (node.name?.startsWith("工具区域")) regions.push(node); });
  assert.ok(regions.length >= 2);
  for (const region of regions) {
    assert.equal(region.width, "fill_container");
    assert.ok(region.padding === undefined || region.padding === 0, region.name);
  }
  assert.ok(rules.includes("工具区域与工具卡 | 宽 `100%`"));
  assert.ok(css.includes(".markdown-table { width: 816px; max-width: 100%; overflow-x: auto; }"));
  assert.ok(css.includes("grid-template-columns: var(--tool-label-width, max-content) minmax(0, 1fr)"));
});

test("登录画布固定三个入口，保留原页面 ID 与字体定义", () => {
  for (const id of ["l3sMC", "XuPCA", "fTlPh", "O9NST", "g4P6HD", "PX9Nr", "v5xtE", "cegkr", "g4NGl", "KgODq"]) {
    const root = web.children.find((node) => node.id === id);
    assert.ok(root);
    const tracks = [];
    walk(root, (node) => { if (node.name === "登录方式 · cstoa / OAuth / APIKEY") tracks.push(node); });
    assert.equal(tracks.length, 1);
    assert.deepEqual(tracks[0].children.map((node) => node.children[0].content), ["cstoa", "OAuth", "APIKEY"]);
  }
  assert.equal(web.fonts.length, 3);
  assert.ok(web.fileToken);
});

test("侧栏只动画位移，选区与条目等宽等高，树角色列不收缩", () => {
  assert.ok(css.includes(".sidebar-new, .sidebar-group, .sidebar-session-row { width: 100%; }"));
  assert.ok(css.includes("width: 100%; height: var(--control-menu); visibility: hidden"));
  const sidebar = /\.sidebar \{([^}]+)\}/u.exec(css)[1];
  assert.ok(sidebar.includes("transition: transform"));
  assert.ok(!sidebar.includes("transition: width"));
  assert.ok(css.includes(".tree-role { flex: 0 0 auto; white-space: nowrap;"));
});

test("主页切换拆分背景、品牌、欢迎语、页脚及两处输入区，支持减弱动态", () => {
  for (const name of ["home-background", "home-mark", "home-greeting", "home-footer", "home-composer", "workspace-composer", "workspace-header", "workspace-messages"]) assert.ok(css.includes(`view-transition-name: ${name};`));
  assert.ok(css.includes("transform: translate3d(0, -100%, 0)"));
  assert.match(css, /::view-transition-group\(\*\).*animation-duration: 0s !important/su);
});

test("动效令牌与 DESIGN.md 的 25 项刻度一致", () => {
  const values = {
    "duration-stagger": "40ms", "duration-micro": "80ms", "duration-quick": "150ms",
    "duration-fast": "250ms", "duration-medium": "350ms", "duration-slow": "400ms", "duration-very-slow": "500ms",
    "ease-smooth-out": "cubic-bezier(.22, 1, .36, 1)", "ease-in-out": "ease-in-out", "ease-out": "ease-out",
    "ease-linear": "linear", "ease-bounce": "cubic-bezier(.34, 1.36, .64, 1)", "ease-bounce-strong": "cubic-bezier(.34, 3.85, .64, 1)",
    "distance-micro": "4px", "distance-small": "6px", "distance-base": "8px", "distance-medium": "12px", "distance-large": "30px",
    "scale-large": ".96", "scale-medium": ".97", "scale-small": ".98", "scale-tiny": ".99",
    "blur-small": "2px", "blur-medium": "3px", "blur-large": "8px",
  };
  assert.equal(Object.keys(values).length, 25);
  for (const [name, value] of Object.entries(values)) {
    assert.ok(css.includes(`--${name}: ${value};`), name);
    assert.ok(rules.includes(`\`--${name}\``), name);
  }
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)/u);
});
