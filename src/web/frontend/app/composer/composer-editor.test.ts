import assert from "node:assert/strict";
import { test } from "node:test";
import {
	chipCaretSpace,
	editorHtml,
	extractPayload,
	fileReference,
	parseEditor,
	stringifyEditor,
} from "./composer-editor.ts";

const commands = ["/compact", "/fork", "/tree", "/skill:disk"];

test("plain text survives a round trip untouched", () => {
	const value = "帮我把日志压缩一下";
	assert.deepEqual(parseEditor(value, commands), [{ kind: "text", text: value }]);
	assert.equal(stringifyEditor(parseEditor(value, commands)), value);
});

test("leading command token becomes a chip", () => {
	const segments = parseEditor("/compact 把最近两小时的日志压缩一下", commands);
	assert.deepEqual(segments, [
		{ kind: "chip", token: "/compact", label: "compact", icon: "command" },
		{ kind: "text", text: "把最近两小时的日志压缩一下" },
	]);
	assert.equal(stringifyEditor(segments), "/compact 把最近两小时的日志压缩一下");
});

test("skill command chip shows the skill name without its prefix", () => {
	const [chip] = parseEditor("/skill:disk 体检", commands);
	assert.deepEqual(chip, { kind: "chip", token: "/skill:disk", label: "disk", icon: "command" });
});

test("unknown slash words stay plain text", () => {
	const value = "/unknown thing";
	assert.deepEqual(parseEditor(value, commands), [{ kind: "text", text: value }]);
});

test("file token becomes a chip anywhere after whitespace", () => {
	const segments = parseEditor("@磁盘体检.txt 帮我看看这个文件", commands);
	assert.deepEqual(segments, [
		{ kind: "chip", token: "@磁盘体检.txt", label: "磁盘体检.txt", icon: "file" },
		{ kind: "text", text: " 帮我看看这个文件" },
	]);
});

test("file chip displays filename while preserving full path and hover title", () => {
	const token = fileReference("logs/windows/events.txt");
	const [chip] = parseEditor(token, commands);
	assert.deepEqual(chip, { kind: "chip", token, label: "events.txt", icon: "file" });
	assert.match(editorHtml([chip]), /title="@logs\/windows\/events.txt"/u);
	assert.equal(extractPayload(token, commands).body, token);
});

test("file paths with spaces remain one chip after draft restoration", () => {
	const token = fileReference("日志/设备 诊断.txt");
	const segments = parseEditor(`${token} 请检查`, commands);
	assert.deepEqual(segments[0], { kind: "chip", token, label: "设备 诊断.txt", icon: "file" });
	assert.equal(stringifyEditor(segments), `${token} 请检查`);
	assert.equal(extractPayload(`${token} 请检查`, commands).body, `${token} 请检查`);
});

test("leading whitespace before a file reference survives restoration", () => {
	for (const value of [" @a.txt", "\n@a.txt", "\t@a.txt 说明"]) {
		assert.equal(stringifyEditor(parseEditor(value, commands)), value);
	}
});

test("unfinished quoted file references remain plain text", () => {
	for (const value of ['@"设备 日志', '@"日志\\q.txt"']) {
		assert.equal(stringifyEditor(parseEditor(value, commands)), value);
	}
});

test("email-style @ inside a word stays plain text", () => {
	const value = "联系 a@b.com 获取";
	const segments = parseEditor(value, commands);
	assert.deepEqual(segments, [{ kind: "text", text: value }]);
});

test("extractPayload splits command, files and body", () => {
	const result = extractPayload("/compact 参考 @磁盘体检.txt 和 @驱动.txt 再压缩", commands);
	assert.equal(result.command, "/compact");
	assert.equal(result.body, "参考 @磁盘体检.txt 和 @驱动.txt 再压缩");
});

test("extractPayload keeps the first command chip only", () => {
	const result = extractPayload("/compact /tree 文本", commands);
	assert.equal(result.command, "/compact");
	assert.equal(result.body, "/tree 文本");
});

test("extractPayload without command sends body as-is", () => {
	const result = extractPayload(" plain body ", commands);
	assert.equal(result.command, "");
	assert.equal(result.body, "plain body");
});

test("editorHtml renders chips with tokens and escapes text", () => {
	const html = editorHtml(parseEditor("/compact <b>", commands));
	assert.match(html, /data-token="\/compact"/u);
	assert.match(html, /composer-chip-label">compact</u);
	assert.match(html, /&lt;b&gt;/u);
	assert.match(html, /chip-remove/u);
});

test("chips end with an editable caret gap without changing serialized tokens", () => {
	for (const token of ["/compact", "/skill:disk", "@a.txt"]) {
		const segments = parseEditor(token, commands);
		assert.ok(editorHtml(segments).endsWith(chipCaretSpace));
		assert.equal(stringifyEditor(segments), token);
	}
});

test("file separator uses the caret gap while extra user spaces remain", () => {
	const html = editorHtml(parseEditor("@a.txt  正文", commands));
	assert.ok(html.endsWith(`${chipCaretSpace} 正文`));
});

test("multiline text survives a round trip", () => {
	const value = "第一段\n第二段 @a.txt\n第三段";
	const segments = parseEditor(value, commands);
	assert.equal(stringifyEditor(segments), value);
	const html = editorHtml(segments);
	assert.match(html, /第一段<br>第二段/u);
	assert.match(html, /data-token="@a\.txt"/u);
});
