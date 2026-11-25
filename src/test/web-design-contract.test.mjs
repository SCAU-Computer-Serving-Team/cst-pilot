import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const desktopCanvas = JSON.parse(readFileSync(new URL("../../design/cst-pilot-desktop.pen", import.meta.url), "utf8"));
const webCanvas = JSON.parse(readFileSync(new URL("../../design/cst-pilot-web.pen", import.meta.url), "utf8"));

function descendants(node) {
  return [node, ...(node.children ?? []).flatMap(descendants)];
}

function overlapsRect(a, b) {
  return (
    typeof a.width === "number" &&
    typeof a.height === "number" &&
    typeof b.width === "number" &&
    typeof b.height === "number" &&
    a.x < b.x + b.width &&
    a.x + a.width > b.x &&
    a.y < b.y + b.height &&
    a.y + a.height > b.y
  );
}

test("桌面与 Web 画布的节点 ID 唯一，工作帧没有遗留占位标记", () => {
  for (const [file, canvas] of [
    ["cst-pilot-desktop.pen", desktopCanvas],
    ["cst-pilot-web.pen", webCanvas],
  ]) {
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

test("ADR-013：Web 与桌面画布共享 50 项静态变量与浅深双态主题轴", () => {
  assert.deepEqual(webCanvas.themes, { mode: ["light", "dark"] });
  assert.deepEqual(desktopCanvas.themes, { mode: ["light", "dark"] });
  assert.equal(Object.keys(webCanvas.variables).length, 50);
  assert.deepEqual(webCanvas.variables, desktopCanvas.variables);
});

test("Web 画布顶层按 00A–00F/00H 母版与 01–07 业务区收纳且分区不重叠", () => {
  const topNames = webCanvas.children.map((node) => node.name);
  for (const required of [
    "设计规范",
    "00A · 窗口与导航",
    "00B · 按钮与输入",
    "00C · 菜单与弹窗",
    "00D · 列表与消息",
    "00E · 工具与特殊工具",
    "00F · 背景着色器",
    "00H · Markdown 富文本",
  ]) {
    assert.ok(topNames.includes(required), `缺少顶层板：${required}`);
  }

  const sections = webCanvas.children
    .filter((node) => /^0[1-7] ·/.test(node.name ?? ""))
    .sort((a, b) => a.name.localeCompare(b.name));
  assert.deepEqual(
    sections.map((node) => node.name),
    [
      "01 · 工作台",
      "02 · 会话管理",
      "03 · 设置",
      "04 · 账号与登录",
      "05 · 深色模式部分参考",
      "06 · 边界情况",
      "07 · 工具与特殊工具",
    ],
  );
  assert.deepEqual(
    sections.map((section) => section.children.slice(1).map((group) => group.name)),
    [
      ["主页", "对话", "对话中的工具", "状态提示与通道状态", "输入与快捷面板", "仪表盘"],
      ["全部会话", "分支树", "会话导出", "侧栏搜索"],
      ["常规", "提供商", "模型偏好", "快捷键"],
      ["登录", "CSTOA 登录", "账号"],
      ["深色模式部分参考"],
      ["响应式断点与长文本"],
      ["普通工具", "整机概览", "BitLocker", "命令清单", "联网检索"],
    ],
  );
  for (const section of sections) {
    assert.equal(section.children[0].name, "主区标题栏", `${section.name} 缺少主区标题栏`);
    for (const other of sections.filter((node) => node !== section)) {
      assert.equal(overlapsRect(section, other), false, `${section.name} 与 ${other.name} 重叠`);
    }
  }
});

test("输入栏与模型选择器不残留旧版独立 Select 控件", () => {
  const allNodes = descendants(webCanvas);
  for (const forbidden of [
    "HeroUI Select · 模型",
    "HeroUI Select · 思考强度",
    "上下文进度环",
    "思考强度滑块 · Low High Max",
  ]) {
    assert.equal(
      allNodes.some((node) => node.name === forbidden),
      false,
      `不应残留旧版控件：${forbidden}`,
    );
  }
  const composerMaster = allNodes.find((node) => node.id === "ZWAs1");
  assert.ok(composerMaster?.reusable, "缺少消息输入框母版 ZWAs1");
  const inputGroup = webCanvas.children
    .find((node) => node.name === "01 · 工作台")
    .children.find((node) => node.name === "输入与快捷面板");
  assert.ok(
    inputGroup.children.some((node) => node.name === "输入框 · 模型选择与额度"),
    "缺少模型选择与额度工作帧",
  );
});

test("会话导出工作帧齐全、开启裁切且彼此不重叠", () => {
  const section = webCanvas.children
    .find((node) => node.name === "02 · 会话管理")
    .children.find((node) => node.name === "会话导出");
  assert.ok(section);
  for (const frameName of ["会话导出 · 预览", "会话导出 · 保存完成", "会话导出 · 生成中与失败"]) {
    const page = section.children.find((node) => node.name === frameName);
    assert.ok(page, `缺少导出页工作帧：${frameName}`);
    assert.equal(page.clip, true);
    const nodes = descendants(page);
    assert.equal(
      nodes.some((node) => ["归档日期字段", "范围字段", "保存位置与提醒", "格式说明"].includes(node.name)),
      false,
      `${frameName} 不应包含冗余表单项或辅助说明`,
    );
    assert.ok(
      nodes.some((node) => node.name === "生成并保存按钮"),
      `${frameName} 缺少生成并保存按钮`,
    );
    assert.ok(
      nodes.some((node) => node.name === "会话文档预览视口"),
      `${frameName} 缺少会话文档预览视口`,
    );
    const overlaps = section.children.filter((other) => other !== page && overlapsRect(page, other));
    assert.deepEqual(overlaps.map((node) => node.name), [], `${frameName} 与其他画布重叠`);
  }
});

test("设置分区四类子页齐全，自定义提供商与模型偏好互不重叠且无文本占位符号", () => {
  const settings = webCanvas.children.find((node) => node.name === "03 · 设置");
  assert.ok(settings);
  const groups = settings.children.slice(1);
  for (const group of groups) {
    const boards = (group.children ?? []).filter((node) => node.clip === true);
    assert.ok(boards.length >= 1, `设置分组 ${group.name} 缺少页面画布`);
    for (const board of boards) {
      const overlaps = boards.filter((other) => other !== board && overlapsRect(board, other));
      assert.deepEqual(overlaps.map((node) => node.name), [], `${board.name} 与同组画布重叠`);
    }
  }
  const providers = settings.children.find((node) => node.name === "提供商");
  const customPage = providers.children.find((node) => node.name === "提供商 · 自定义提供商");
  assert.ok(customPage, "缺少提供商 · 自定义提供商画布");
  for (const node of descendants(customPage)) {
    assert.notEqual(node.content?.includes("□"), true, "能力项不使用文本勾选框");
    assert.notEqual(node.content?.includes("▾"), true, "下拉指示不使用文本符号");
  }
});
