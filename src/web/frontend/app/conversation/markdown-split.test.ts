import assert from "node:assert/strict";
import { test } from "node:test";
import { splitStableText } from "./markdown-split.ts";

test("普通段落按双换元切分，尾段为最后一段", () => {
	const { stable, tail } = splitStableText("第一段。\n\n第二段。\n\n第三段（流式中）。");
	assert.equal(stable, "第一段。\n\n第二段。");
	assert.equal(tail, "第三段（流式中）。");
});

test("无双换行或只有一段时不切分", () => {
	assert.deepEqual(splitStableText("单段流式文本"), { stable: "", tail: "单段流式文本" });
	assert.deepEqual(splitStableText(""), { stable: "", tail: "" });
});

test("未闭合代码块整体留在尾段", () => {
	const text = "说明：\n\n```bash\necho hi";
	const { stable, tail } = splitStableText(text);
	assert.equal(stable, "");
	assert.equal(tail, text);
});

test("已闭合代码块可以进入前缀，后续新段落作尾段", () => {
	const text = "说明：\n\n```bash\necho hi\n```\n\n收尾一段（流式中）";
	const { stable, tail } = splitStableText(text);
	assert.equal(stable, "说明：\n\n```bash\necho hi\n```");
	assert.equal(tail, "收尾一段（流式中）");
});

test("列表项间的空行是列表延续，不切分", () => {
	const text = "前言。\n\n- 第一项\n\n- 第二项（流式中）";
	const { stable, tail } = splitStableText(text);
	assert.equal(stable, "");
	assert.equal(tail, text);
});

test("有序列表跨空行不切分，避免编号重置", () => {
	const text = "1. 第一\n\n2. 第二（流式中）";
	assert.deepEqual(splitStableText(text), { stable: "", tail: text });
});

test("表格行不是分割候选：表格后接新段落才可切", () => {
	const table = "| a | b |\n|---|---|\n| 1 | 2 |";
	const text = `${table}\n\n表后新段（流式中）`;
	const { stable, tail } = splitStableText(text);
	assert.equal(stable, table);
	assert.equal(tail, "表后新段（流式中）");
});

test("表格与表格之间不切", () => {
	const text = "| a | b |\n|---|---|\n\n| c | d |\n|---|---|（流式中）";
	assert.deepEqual(splitStableText(text), { stable: "", tail: text });
});

test("列表整体定稿后，后续普通新段可切分", () => {
	const text = "- 第一项\n\n- 第二项\n\n普通新段（流式中）";
	const { stable, tail } = splitStableText(text);
	assert.equal(stable, "- 第一项\n\n- 第二项");
	assert.equal(tail, "普通新段（流式中）");
});
