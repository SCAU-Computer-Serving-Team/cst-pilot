import assert from "node:assert/strict";
import { test } from "node:test";
import { presentError } from "./error-presenter.ts";

test("presentError hides raw ENOSPC and system error codes from primary copy while preserving detail", () => {
	const result = presentError("Error: ENOSPC: no space left on device, write", { input: true });
	assert.equal(result.title, "保存失败");
	assert.equal(result.description, "输入已保留，请检查空间或权限。");
	assert.ok(!result.title.includes("ENOSPC") && !result.description.includes("ENOSPC"));
	assert.match(result.detail, /ENOSPC/);
});

test("presentError maps image budget and export height errors to direct Chinese guidance", () => {
	const imageErr = presentError("图片总大小不能超过 12 MiB", { input: true });
	assert.equal(imageErr.title, "图片总大小不能超过 12 MiB");
	assert.match(imageErr.description, /草稿/);

	const tallErr = presentError("export_too_tall: 文档高度超过 100000px");
	assert.equal(tallErr.title, "导出内容过长");
	assert.match(tallErr.description, /HTML/);
});
