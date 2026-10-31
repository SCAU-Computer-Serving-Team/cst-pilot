import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

test("界面画布的节点 ID 唯一，工作帧没有遗留占位标记", () => {
  for (const file of ["cst-pilot-web.pen", "cst-pilot-tools.pen"]) {
    const canvas = JSON.parse(readFileSync(new URL(`../web/design/${file}`, import.meta.url), "utf8"));
    const ids = new Set();
    function visit(node) {
      assert.ok(!ids.has(node.id), `${file} 重复 ID：${node.id}`);
      ids.add(node.id);
      assert.notEqual(node.placeholder, true, `${file} 未完成工作帧：${node.name}`);
      for (const child of node.children ?? []) visit(child);
    }
    for (const root of canvas.children) visit(root);
  }
});

test("界面画布顶层按分区收纳，分区之间不重叠", () => {
  const canvas = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
  const sections = canvas.children.filter(node => /^\d{2} ·/.test(node.name ?? "") && node.name !== "00 · 画布导航");
  assert.deepEqual(sections.map(node => node.name), ["01 · 对话", "02 · 会话管理", "03 · 设置与模型", "04 · 账号与登录", "05 · 通用组件与反馈", "06 · 仪表盘"]);
  assert.equal(canvas.children.length, 8);
  assert.deepEqual(sections.map(section => section.children.slice(1).map(group => group.name)), [["主页与工作台", "输入与排队", "分支与派生"], ["全部会话", "批量管理", "会话导出"], ["设置与模型服务"], ["账号信息", "登录页面"], ["悬停与菜单", "通知、错误与退出", "模型与思考强度选择器 · 浅色", "模型与思考强度选择器 · 深色"], ["仪表盘"]]);
  for (const section of sections) {
    assert.equal(section.children[0].name, "主区标题");
    for (const other of sections.filter(node => node !== section)) {
      assert.ok(section.x + section.width <= other.x || other.x + other.width <= section.x || section.y + section.height <= other.y || other.y + other.height <= section.y, `${section.name} 与 ${other.name} 重叠`);
    }
  }
});

test("聊天工作台和组件画布展示合并的模型与思考强度选择器", () => {
  const canvas = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
  function find(node, name) {
    if (node.name === name) return node;
    for (const child of node.children ?? []) {
      const match = find(child, name);
      if (match) return match;
    }
    return undefined;
  }
  const section = find(canvas, "05 · 通用组件与反馈");
  for (const theme of ["浅色", "深色"]) {
    const chat = find(canvas, `聊天工作台 · ${theme}`);
    const trigger = find(chat, "模型与思考强度");
    assert.deepEqual(trigger.children.map(node => node.name), ["当前模型", "当前思考强度", "图标位"]);
    const contextIcon = find(chat, "上下文图标");
    assert.equal(contextIcon.children[0].icon, "menu");
    assert.deepEqual(contextIcon.padding, [4, 0, 0, 0]);
    assert.equal(find(chat, "HeroUI Select · 思考强度"), undefined);
    const board = find(section, `模型与思考强度选择器 · ${theme}`);
    const panel = find(board, `模型与思考强度面板 · ${theme}`);
    assert.equal(panel.width, 224);
    assert.equal(panel.height, 88);
    assert.equal(panel.layout, "vertical");
    assert.equal(panel.padding, 12);
    assert.deepEqual(panel.children.map(row => row.name), ["选择模型入口", "选择思考强度入口"]);
    assert.ok(panel.children.every(row => row.height === 32 && row.gap === 8 && row.justifyContent === "end"));
    assert.ok(panel.children.every(row => row.children[0].fontSize === 14 && row.children[0].lineHeight === 1.5));
    assert.equal(find(board, "选择思考强度入口").children[0].content, "Max");
    assert.deepEqual(find(board, "模型入口箭头图标位").padding, [4, 0, 0, 0]);
    const modelList = find(board, "模型列表 · 选择状态");
    assert.equal(modelList.gap ?? 0, 0);
    assert.ok(modelList.children.every(row => row.height === 32));
    assert.equal(find(section, "模型列表 · 浅色"), undefined);
    assert.equal(find(section, "模型列表 · 深色"), undefined);
    assert.equal(modelList.width, panel.width);
    assert.ok(modelList.children.every(row => row.children[0].fontSize === 14 && row.children[0].lineHeight === 1.5));
    assert.equal(modelList.y, panel.y);
    const example = find(board, "消息编辑器 · 收起示例");
    const source = find(chat, "HeroUI Textarea · 底部消息编辑器");
    function withoutIds(node) {
      const rest = { ...node };
      delete rest.id;
      if (node.children) rest.children = node.children.map(withoutIds);
      return rest;
    }
    assert.deepEqual(withoutIds({ ...example, name: source.name, x: source.x, y: source.y }), withoutIds(source));
    const thinkingList = find(board, "思考强度列表 · 选择状态");
    assert.equal(thinkingList.width, panel.width);
    assert.equal(thinkingList.padding, modelList.padding);
    assert.deepEqual(thinkingList.children.map(row => row.children[0].content), ["Low", "High", "Max"]);
    assert.equal(thinkingList.children[2].fill, modelList.children[0].fill);
    assert.ok(thinkingList.children.every(row => row.children.length === 1));
    for (const list of [modelList, thinkingList]) {
      assert.ok(list.children.every(row => row.justifyContent === "end" && (row.gap ?? 0) === 0 && row.height === 32));
      assert.ok(list.children.every(row => row.children[0].textAlign === "right" && row.children[0].fontSize === 14));
      assert.ok(list.children.every(row => row.children.length === 1 && row.children[0].type === "text"));
    }
  }
});

test("所有输入栏使用合并的模型与思考强度入口", () => {
  const canvas = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
  let count = 0;
  function visit(node) {
    assert.notEqual(node.name, "HeroUI Select · 模型");
    assert.notEqual(node.name, "HeroUI Select · 思考强度");
    assert.notEqual(node.name, "上下文进度环");
    assert.notEqual(node.name, "思考强度滑块 · Low High Max");
    if (node.name === "上下文与模型选择") {
      count++;
      assert.deepEqual(node.children.map(child => child.name), ["上下文图标", "模型与思考强度"]);
      assert.equal(node.children[0].children[0].icon, "menu");
      assert.deepEqual(node.padding, [0, 8]);
      assert.equal(node.children[1].gap, 6);
      assert.deepEqual(node.children[1].children.slice(0, 2).map(child => child.name), ["当前模型", "当前思考强度"]);
      assert.ok(node.children[1].children.slice(0, 2).every(child => child.fontSize === 13 && child.lineHeight === 1.5));
    }
    for (const child of node.children ?? []) visit(child);
  }
  visit(canvas);
  assert.equal(count, 34);
});

test("会话导出预览与保存结果有浅深画布且不覆盖其他分区", () => {
  const canvas = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
  const section = canvas.children.find(node => node.name === "02 · 会话管理").children.find(node => node.name === "会话导出");
  assert.ok(section);
  for (const state of ["预览", "保存结果"]) for (const theme of ["浅色", "深色"]) {
    const page = section.children.find(node => node.name === `会话导出 · ${state} · ${theme}`);
    assert.ok(page, "导出页状态画布齐全");
    assert.equal(page.clip, true);
    const nodes = [];
    function collect(node) { nodes.push(node); for (const child of node.children ?? []) collect(child); }
    collect(page);
    assert.equal(nodes.some(node => ["归档日期字段", "范围字段"].includes(node.name)), false, "导出时自动确定日期与当前会话状态");
    assert.equal(nodes.find(node => node.name === "生成并保存按钮").cornerRadius, "$radius-control");
    assert.equal(nodes.find(node => node.name === "会话文档").stroke, theme === "浅色" ? "#8A8A8A" : "#77838A");
    assert.equal(nodes.some(node => ["保存位置与提醒", "格式说明"].includes(node.name)), false, "选项区不显示辅助说明");
    const overlaps = section.children.filter(other => other !== page && typeof other.width === "number" && typeof other.height === "number" && page.x < other.x + other.width && page.x + page.width > other.x && page.y < other.y + other.height && page.y + page.height > other.y);
    assert.deepEqual(overlaps.map(node => node.name), []);
  }
});

test("模型选择与协议菜单的浅深主题使用独立画布", () => {
  const canvas = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
  const section = canvas.children.find(node => node.name === "03 · 设置与模型").children.find(node => node.name === "设置与模型服务");
  assert.ok(section);
  for (const prefix of ["默认模型选择 · 展开与未配置", "自定义协议菜单"]) {
    const boards = section.children.filter(node => node.name?.startsWith(prefix));
    assert.deepEqual(boards.map(node => node.name).sort(), [`${prefix} · 浅色`, `${prefix} · 深色`].sort());
    assert.notEqual(boards[0].fill, boards[1].fill, "每张画布使用对应主题的表面色");
  }
  for (const page of section.children.filter(node => ["模型偏好 · light", "模型偏好 · dark"].includes(node.name))) {
    function visit(node) {
      if (node.name === "默认模型选择器") {
        assert.equal(node.children.filter(child => child.type === "text").length, 1, "闭合控件只显示模型名");
      }
      if (node.name === "服务选择器") {
        assert.equal(node.width, 74, "全部筛选保持紧凑宽度");
        assert.equal(node.justifyContent, "space_between", "筛选文字和箭头两侧对齐");
      }
      for (const child of node.children ?? []) visit(child);
    }
    visit(page);
  }
});

test("自定义服务浅深画布归属设置区，与下方分区不重叠", () => {
  const canvas = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
  const domain = canvas.children.find(node => node.name === "03 · 设置与模型");
  const section = domain.children.find(node => node.name === "设置与模型服务");
  assert.ok(section);
  const pages = section.children.filter(node => /^自定义服务 · (light|dark)$/.test(node.name ?? ""));
  assert.equal(pages.length, 2);
  const nextSection = canvas.children.find(node => node.name === "06 · 仪表盘");
  for (const page of pages) {
    assert.equal(domain.y + section.y + page.y, 5152);
    assert.ok(domain.y + section.y + page.y + page.height + 120 <= nextSection.y, "自定义服务与下方分区保留间距");
    const overlaps = section.children.filter(other => other !== page && typeof other.width === "number" && typeof other.height === "number" && page.x < other.x + other.width && page.x + page.width > other.x && page.y < other.y + other.height && page.y + page.height > other.y);
    assert.deepEqual(overlaps.map(node => node.name), [], `${page.name} 与其他画布重叠`);
    function visit(node) {
      assert.notEqual(node.content?.includes("□"), true, "能力项使用胶囊，不使用文本勾选框");
      assert.notEqual(node.content?.includes("▾"), true, "下拉指示使用图标");
      if (node.name === "下拉指示") assert.equal(node.icon, "chevron-down");
      assert.ok(!node.name?.endsWith("· 胶囊"), "能力项为文字与独立按钮");
      if (node.type === "frame" && node.name?.endsWith("· 输入框")) {
        assert.equal(node.height, node.name.startsWith("接口协议") ? 32 : 42);
        for (const child of node.children ?? []) if (child.type === "text") assert.equal(child.fontSize, node.name.startsWith("接口协议") ? 14 : 16);
      }
      if (node.name?.endsWith("· 启用按钮")) {
        assert.equal(node.width, 72);
        assert.equal(node.height, 32);
        const radius = typeof node.cornerRadius === "string" && node.cornerRadius.startsWith("$")
          ? canvas.variables[node.cornerRadius.slice(1)].value
          : node.cornerRadius;
        assert.equal(radius, 4);
      }
      for (const child of node.children ?? []) visit(child);
    }
    visit(page);
  }
});
