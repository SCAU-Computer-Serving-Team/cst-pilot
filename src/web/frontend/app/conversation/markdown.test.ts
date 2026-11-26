import assert from "node:assert/strict";
import { test } from "node:test";
import { splitStreamingMarkdownBlocks } from "./markdown-blocks.ts";

test("splitStreamingMarkdownBlocks 仅在围栏代码块外部按空行切分定稿块", () => {
	const single = splitStreamingMarkdownBlocks("正在生成第一段");
	assert.deepEqual(single, { settled: [], tail: "正在生成第一段" });

	const multi = splitStreamingMarkdownBlocks("第一段完成。\n\n第二段正在流式输出");
	assert.deepEqual(multi, {
		settled: ["第一段完成。"],
		tail: "第二段正在流式输出",
	});

	const fenced = splitStreamingMarkdownBlocks("前言\n\n```ts\nconst a = 1;\n\nconst b = 2;\n```");
	assert.deepEqual(fenced, {
		settled: ["前言"],
		tail: "```ts\nconst a = 1;\n\nconst b = 2;\n```",
	});

	const afterFence = splitStreamingMarkdownBlocks("```ts\nconst a = 1;\n\nconst b = 2;\n```\n\n结语");
	assert.deepEqual(afterFence, {
		settled: ["```ts\nconst a = 1;\n\nconst b = 2;\n```"],
		tail: "结语",
	});
});
