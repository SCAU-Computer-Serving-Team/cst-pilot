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

test("画布导航覆盖十个主题，浅深配置帧相邻排列", () => {
  const navigation = web.children.find((node) => node.name === "00 · 画布导航");
  assert.ok(navigation);
  assert.equal(web.children.filter((node) => node.name?.includes("分区标题")).length, 10);
  const entries = [];
  walk(navigation, (node) => { if (node.content) entries.push(node.content); });
  assert.ok(entries.some((content) => content.includes("会话批量管理")));
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

test("会话勾选无多余外框，错误与退出按组件分组", () => {
  let checkRows = 0;
  for (const root of web.children) walk(root, (node) => {
    assert.notEqual(node.name, "会话勾选图标位", "16×16勾选框直接参与行布局");
    if (node.children?.some((child) => child.type === "ref" && /会话[未已]勾选/.test(child.name ?? ""))) {
      checkRows++;
      const check = node.children.find((child) => child.type === "ref" && /会话[未已]勾选/.test(child.name ?? ""));
      if (node.layout === "none") {
        const text = node.children.find((child) => child.type === "text");
        assert.equal(check.y + check.height / 2, text.y + text.height / 2, node.id);
      } else assert.equal(node.alignItems, "center");
      assert.equal(check.width, 16);
      assert.equal(check.height, 16);
    }
  });
  assert.ok(checkRows >= 10);
  for (const id of ["q6c4dk", "xsI8b"]) {
    const board = web.children.find((node) => node.id === id);
    assert.equal(board.height, 1100);
    for (const name of ["错误与恢复 · 组件分组", "技术详情 · 组件分组", "退出确认 · 组件分组", "退出完成 · 状态示例"]) assert.ok(board.children.some((node) => node.name === name));
    assert.ok(!board.children.some((node) => node.name === "交互规范" || node.name === "空闲退出规则"));
    const cards = [];
    walk(board, (node) => { if (["认证失效", "写入失败", "模型失败", "断线横幅"].includes(node.name)) cards.push(node); });
    assert.equal(cards.length, 4);
    for (const card of cards) {
      assert.equal(card.width, 520);
      assert.equal(card.height, undefined);
      assert.equal(card.padding, 16);
      if (id === "q6c4dk") {
        assert.equal(card.fill, "#FCFCFC");
        assert.equal(card.stroke, "#E0E0E0");
      }
      const titles = [];
      walk(card, (node) => { if (node.name === "主提示") titles.push(node.content); });
      assert.ok(titles.every((title) => title.length <= 6), "主提示只写状态");
    }
    walk(board, (node) => {
      if (node.name === "保存说明") assert.equal(node.content, "记录保留，任务不自动重放。");
      if (id === "q6c4dk" && node.type === "frame") {
        assert.notEqual(node.fill, "#F6F6F6", "错误与退出使用近白底色");
        if (typeof node.fill === "string" && /^#[0-9A-F]{6}$/i.test(node.fill)) {
          assert.equal(node.fill.slice(1, 3), node.fill.slice(3, 5), "底色无红绿偏向");
          assert.equal(node.fill.slice(3, 5), node.fill.slice(5, 7), "底色无蓝色偏向");
        }
      }
    });
  }
});

test("弹出通知的浅深组件、顶部位置与堆叠共用规范", () => {
  for (const id of ["Ohy8e", "c6cYjn"]) {
    const board = web.children.find((node) => node.id === id);
    assert.ok(board);
    const component = board.children.find((node) => node.name?.startsWith("组件 · 弹出通知"));
    assert.equal(component.reusable, true);
    assert.deepEqual(component.padding, [8, 16]);
    assert.equal(component.height, 44);
    assert.equal(component.gap, 12);
    assert.equal(component.cornerRadius, 8);
    assert.equal(component.children.find((node) => node.name === "关闭通知按钮").width, 26);
    assert.equal(board.children.filter((node) => node.type === "ref" && node.ref === component.id).length, 3);
    const preview = board.children.find((node) => node.name.startsWith("960窗口"));
    const region = preview.children.find((node) => node.name === "统一通知区域");
    assert.equal(region.y, 20);
    assert.equal(region.x + region.width / 2, preview.width / 2);
    assert.equal(region.gap, 8);
  }
  for (const root of web.children) walk(root, (node) => {
    if (node.id === "iHGJ6" || node.id === "bg3n1" || node.name === "消息弹窗") {
      assert.equal(node.y, 20);
      assert.deepEqual(node.padding, [8, 16]);
      assert.equal(node.cornerRadius, 8);
    }
  });
  const regionCss = /\.notification-region \{([^}]+)\}/u.exec(css)[1];
  assert.ok(regionCss.includes("position: fixed"));
  assert.ok(regionCss.includes("top: 20px"));
  assert.ok(regionCss.includes("left: 50%"));
  assert.ok(regionCss.includes("flex-direction: column"));
  assert.ok(regionCss.includes("gap: var(--space-inline)"));
  assert.ok(rules.includes("## 弹出通知"));
});

test("账号页只保留身份字段，CSTOA 额度只放上下文面板", () => {
  for (const id of ["bOVBg", "UsSrK", "XmU65", "iUIzN"]) {
    const fields = [];
    walk(web.children.find((node) => node.id === id), (node) => { if (node.name?.startsWith("账号字段 ·")) fields.push(node.name); });
    assert.deepEqual(fields, ["账号字段 · 学号", "账号字段 · 姓名"]);
  }
  for (const id of ["oVwiy", "z0lQt"]) {
    const root = web.children.find((node) => node.id === id), values = [];
    assert.ok(root.name.includes("CSTOA"));
    assert.equal(root.padding, 24);
    walk(root, (node) => { if (node.content) values.push(node.content); });
    assert.ok(values.includes("12,345 额度点"));
    assert.ok(values.includes("账号信息"));
  }
  for (const id of ["CMIkr", "l0Wrr4"]) {
    const rows = [];
    walk(web.children.find((node) => node.id === id), (node) => { if (node.name === "CSTOA 账号身份") rows.push(node); });
    assert.equal(rows.length, 1);
    assert.ok(!JSON.stringify(rows).includes("额度"));
  }
});
