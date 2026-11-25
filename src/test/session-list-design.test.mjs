import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const canvas = JSON.parse(readFileSync(new URL("../../design/cst-pilot-web.pen", import.meta.url), "utf8"));

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

test("侧边栏母版定义完整，会话管理画布统一复用侧栏母版", () => {
  const nav = canvas.children.find((node) => node.name === "00A · 窗口与导航");
  assert.ok(nav);
  const sidebar = descendants(nav).find((node) => node.id === "L081M1");
  const batchSidebar = descendants(nav).find((node) => node.id === "h5AY3");
  assert.ok(sidebar?.reusable, "缺少侧边栏母版 L081M1");
  assert.ok(batchSidebar?.reusable, "缺少多选侧边栏母版 h5AY3");
  assert.deepEqual(
    sidebar.children.map((child) => child.name),
    ["侧栏上部容器", "账户与设置"],
  );

  const sessionMgmt = canvas.children.find((node) => node.name === "02 · 会话管理");
  const sidebarRefs = descendants(sessionMgmt).filter(
    (node) => node.type === "ref" && (node.ref === "L081M1" || node.ref === "h5AY3"),
  );
  assert.ok(sidebarRefs.length >= 10, "会话管理各工作帧应复用统一侧边栏母版");
});

test("全部会话分区包含分组列表、批量管理、删除确认、空态与 409 回滚画布且不重叠", () => {
  const section = canvas.children
    .find((node) => node.name === "02 · 会话管理")
    .children.find((node) => node.name === "全部会话");
  assert.ok(section);

  const expectedFrames = [
    "全部会话 · 分组列表",
    "全部会话 · 批量管理",
    "全部会话 · 删除确认",
    "全部会话 · 空列表",
    "侧栏 · 最近多选管理",
    "全部会话 · 重命名弹窗",
    "全部会话 · 批量删除部分冲突 (409)",
  ];
  for (const name of expectedFrames) {
    const page = section.children.find((node) => node.name === name);
    assert.ok(page, `缺少画布：${name}`);
    assert.equal(page.clip, true);
    assert.equal(page.width, 1280);
    assert.equal(page.height, 800);
    const overlaps = section.children.filter((other) => other !== page && overlapsRect(page, other));
    assert.deepEqual(overlaps.map((node) => node.name), [], `${name} 与其他画布重叠`);
  }

  const listPage = section.children.find((node) => node.name === "全部会话 · 分组列表");
  const listTexts = descendants(listPage)
    .filter((node) => node.type === "text")
    .map((node) => node.content);
  for (const heading of ["今天", "最近一周", "最近一个月", "管理"]) {
    assert.ok(listTexts.includes(heading), `全部会话 · 分组列表缺少文本：${heading}`);
  }
});

test("ADR-013：特殊工具母版已合并至 Web 单画布，整机概览与导出页不使用旧版简化表格", () => {
  const toolMasters = canvas.children.find((node) => node.name === "00E · 工具与特殊工具");
  assert.ok(toolMasters);
  const reusables = descendants(toolMasters).filter((node) => node.reusable);
  for (const [id, name] of [
    ["wmkJw", "特殊工具 · 整机概览"],
    ["JQMQZ", "特殊工具 · 联网检索"],
    ["s3WzY", "特殊工具 · 命令清单"],
    ["ABn8j", "特殊工具 · BitLocker"],
  ]) {
    assert.ok(
      reusables.some((node) => node.id === id && node.name === name),
      `00E 缺少可复用母版：${name}(${id})`,
    );
  }

  const overviewGroup = canvas.children
    .find((node) => node.name === "07 · 工具与特殊工具")
    .children.find((node) => node.name === "整机概览");
  assert.ok(overviewGroup);
  for (const frameName of ["整机概览 · 采集中与默认收起", "整机概览 · 展开详情与部分缺失", "整机概览 · 深色"]) {
    const board = overviewGroup.children.find((node) => node.name === frameName);
    assert.ok(board, `缺少整机概览工作帧：${frameName}`);
    assert.equal(board.width, 1280);
    const overlaps = overviewGroup.children.filter((other) => other !== board && overlapsRect(board, other));
    assert.deepEqual(overlaps.map((node) => node.name), [], `${frameName} 与其他画布重叠`);
  }

  const exportGroup = canvas.children
    .find((node) => node.name === "02 · 会话管理")
    .children.find((node) => node.name === "会话导出");
  assert.equal(
    descendants(exportGroup).some((node) => node.name === "检查表格"),
    false,
    "导出页不使用自行简化的检查表格",
  );
});
