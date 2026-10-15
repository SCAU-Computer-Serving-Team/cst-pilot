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
