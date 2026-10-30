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

test("会话导出预览与保存结果有浅深画布且不覆盖其他分区", () => {
  const canvas = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
  for (const state of ["预览", "保存结果"]) for (const theme of ["浅色", "深色"]) {
    const page = canvas.children.find(node => node.name === `会话导出 · ${state} · ${theme}`);
    assert.ok(page, "导出页状态画布齐全");
    assert.equal(page.clip, true);
    const overlaps = canvas.children.filter(other => other !== page && typeof other.width === "number" && typeof other.height === "number" && page.x < other.x + other.width && page.x + page.width > other.x && page.y < other.y + other.height && page.y + page.height > other.y);
    assert.deepEqual(overlaps.map(node => node.name), []);
  }
});

test("模型选择与协议菜单的浅深主题使用独立画布", () => {
  const canvas = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
  for (const prefix of ["默认模型选择 · 展开与未配置", "自定义协议菜单"]) {
    const boards = canvas.children.filter(node => node.name?.startsWith(prefix));
    assert.deepEqual(boards.map(node => node.name).sort(), [`${prefix} · 浅色`, `${prefix} · 深色`].sort());
    assert.notEqual(boards[0].fill, boards[1].fill, "每张画布使用对应主题的表面色");
  }
  for (const page of canvas.children.filter(node => ["模型偏好 · light", "模型偏好 · dark"].includes(node.name))) {
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
  const pages = canvas.children.filter(node => /^自定义服务 · (light|dark)$/.test(node.name ?? ""));
  assert.equal(pages.length, 2);
  const loginSection = canvas.children.find(node => node.name?.startsWith("05 ·"));
  for (const page of pages) {
    assert.equal(page.y, 4976);
    assert.ok(page.y + page.height + 120 <= loginSection.y, "自定义服务与下方分区保留间距");
    const overlaps = canvas.children.filter(other => other !== page && typeof other.width === "number" && typeof other.height === "number" && page.x < other.x + other.width && page.x + page.width > other.x && page.y < other.y + other.height && page.y + page.height > other.y);
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
