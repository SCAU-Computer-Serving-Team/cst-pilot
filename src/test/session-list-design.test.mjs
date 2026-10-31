import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const canvas = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-web.pen", import.meta.url), "utf8"));
function descendants(node) {
  return [node, ...(node.children ?? []).flatMap(descendants)];
}

test("全部会话页有独立浅深画布、四个时间分组与16px会话标题", () => {
  const section = canvas.children.find(node => node.name === "02 · 会话管理").children.find(node => node.name === "全部会话");
  assert.ok(section);
  for (const theme of ["浅色", "深色"]) {
    const page = section.children.find(node => node.name === `全部会话 · ${theme}`);
    assert.ok(page, `缺少全部会话${theme}设计稿`);
    assert.equal(page.clip, true);
    const nodes = descendants(page);
    assert.deepEqual(nodes.filter(node => node.name === "时间分组标题").map(node => node.content), ["今天", "最近一周", "最近一个月", "更早"]);
    const titles = nodes.filter(node => node.name === "会话标题");
    assert.ok(titles.length >= 4);
    for (const title of titles) assert.equal(title.fontSize, 16);
    assert.ok(nodes.some(node => node.content === "管理"));
    const rows = nodes.filter(node => node.name === "会话行");
    assert.ok(rows.every(node => node.height === 40), "会话行统一收紧为40px");
    const dates = nodes.filter(node => node.name === "更新时间");
    assert.equal(dates.length, titles.length, "每条会话都有具体日期");
    assert.ok(dates.every(node => /^\d{4}\/\d{2}\/\d{2} \d{2}:\d{2}$/.test(node.content)));
    assert.equal(nodes.find(node => node.name === "时间分组列表").gap, 16);
    assert.ok(nodes.filter(node => node.name === "分组内会话").every(node => (node.gap ?? 0) === 0));
    const overlaps = section.children.filter(other => other !== page && typeof other.width === "number" && typeof other.height === "number" && page.x < other.x + other.width && page.x + page.width > other.x && page.y < other.y + other.height && page.y + page.height > other.y);
    assert.deepEqual(overlaps.map(node => node.name), []);
  }
});

function componentShape(node, tools, sources, theme = {}, root = false) {
  if (node.type === "ref") {
    const overrides = Object.fromEntries(Object.entries(node).filter(([key]) => !["id", "ref", "type"].includes(key)));
    node = { ...sources.find(source => source.id === node.ref), ...overrides };
  }
  theme = { ...theme, ...node.theme };
  function resolve(value) {
    if (typeof value === "string" && value.startsWith("$")) {
      const variable = tools.variables[value.slice(1)].value;
      return Array.isArray(variable)
        ? resolve(variable.find(entry => Object.entries(entry.theme ?? {}).every(([axis, selected]) => selected === (theme[axis] ?? tools.themes[axis][0]))).value)
        : resolve(variable);
    }
    if (Array.isArray(value)) return value.map(resolve);
    if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, resolve(item)]));
    return value;
  }
  return Object.fromEntries(Object.entries(node).filter(([key]) => !["id", "reusable", "theme"].includes(key) && !(root && ["width", "x", "y", "context"].includes(key))).map(([key, value]) => [key, key === "children" ? value.map(child => componentShape(child, tools, sources, theme)) : resolve(value)]));
}

test("导出预览与保存结果直接复用正式整机概览工具设计", () => {
  const tools = JSON.parse(readFileSync(new URL("../web/design/cst-pilot-tools.pen", import.meta.url), "utf8"));
  const sources = descendants(tools);
  for (const theme of ["浅色", "深色"]) for (const state of ["预览", "保存结果"]) {
    const section = canvas.children.find(node => node.name === "02 · 会话管理").children.find(node => node.name === "会话导出");
    const page = section.children.find(node => node.name === `会话导出 · ${state} · ${theme}`);
    const source = sources.find(node => node.name === `内嵌组件 · sys overview · 整机概览 · ${theme}`);
    assert.ok(source);
    const nodes = descendants(page);
    const component = nodes.find(node => node.name === source.name);
    assert.ok(component, "导出文档须使用正式工具组件");
    assert.deepEqual(componentShape(component, tools, sources, {}, true), componentShape(source, tools, sources, {}, true));
    assert.equal(nodes.some(node => node.name === "检查表格"), false, "不使用自行简化的工具表格");
  }
});
