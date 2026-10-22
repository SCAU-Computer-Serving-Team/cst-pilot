import assert from "node:assert/strict";
import { test } from "node:test";
import { presentError } from "./error-presenter.ts";

test("认证错误提供重新登录且保留技术详情", () => {
	const e = presentError('401 {"error":{"type":"authentication_error","message":"invalid api key"}}');
	assert.equal(e.kind, "auth");
	assert.equal(e.title, "登录已失效");
	assert.equal(e.description, "");
	assert.equal(e.action, "重新登录");
	assert.match(e.detail, /401/);
});
test("写失败、限流与网络错误分类，未知错误不伪造原因", () => {
	assert.equal(presentError("工具包目录无法写入，输入未被接受").kind, "storage");
	assert.equal(presentError("429 rate limit").kind, "rate");
	assert.equal(presentError("Failed to fetch").kind, "connection");
	const e = presentError("provider internal exception");
	assert.equal(e.kind, "general");
	assert.equal(e.detail, "provider internal exception");
});
test("会话占用与配置损坏给出对应恢复步骤", () => {
	const owned = presentError(
		"session_owned: 该会话正由另一端使用，请先关闭持有该会话的 TUI 或 Web 服务，再重新打开。",
	);
	assert.equal(owned.kind, "owner");
	assert.match(owned.description, /关闭.*TUI/);
	const config = presentError("configuration_unavailable: 设置文件无法读取，请检查 settings.json 后重试。");
	assert.equal(config.kind, "configuration");
	assert.match(config.description, /settings.json/);
	assert.equal(config.action, "重试");
});

test("Context-aware copy follows the canvas without claiming unconfirmed writes", () => {
	const storage = presentError("ENOSPC", { input: true });
	assert.equal(storage.title, "保存失败");
	assert.equal(storage.description, "输入已保留，请检查空间或权限。");
	assert.equal(presentError("ENOSPC").description, "请检查空间或权限。");
	const model = presentError("provider internal exception", { model: true });
	assert.equal(model.title, "请求失败");
	assert.equal(model.description, "消息已保存。");
	assert.equal(presentError("provider internal exception").title, "操作未完成");
	const connection = presentError("Failed to fetch");
	assert.equal(connection.title, "连接断开");
	assert.equal(connection.description, "正在重连，输入已保留。");
	const pending = presentError("Failed to fetch", { input: true });
	assert.equal(pending.title, "提交待确认");
	assert.match(pending.description, /恢复后重试核查提交/);
	assert.ok(!pending.description.includes("已保存"));
});

test("压缩状态保留具体原因，与失败和执行中状态分开", () => {
	const empty = presentError("当前没有可压缩的较早内容");
	assert.equal(empty.kind, "compaction");
	assert.equal(empty.title, "暂无可压缩内容");
	const done = presentError("会话已压缩，继续对话后可再次压缩");
	assert.equal(done.kind, "compaction");
	assert.equal(done.title, "会话已压缩");
	assert.equal(presentError("会话正在执行，请等待完成后再压缩").kind, "busy");
	assert.equal(presentError("操作未完成，请稍后重试").kind, "general");
});

test("压缩通知的悬停说明包含原因，兼容无需压缩的返回文案", () => {
	const done = presentError("会话已压缩，继续对话后可再次压缩");
	assert.match(done.description, /上次压缩后没有新增对话/);
	const empty = presentError("当前没有可压缩的较早内容");
	assert.match(empty.description, /保留近期原文/);
	assert.match(empty.description, /不足以分出/);
	const legacy = presentError("当前会话无需压缩");
	assert.equal(legacy.kind, "compaction");
	assert.equal(legacy.title, "暂时无法压缩");
	assert.match(legacy.description, /内容较少/);
	assert.match(legacy.description, /没有新增对话/);
	for (const error of [done, empty, legacy]) {
		assert.ok(!error.description.includes("检查设置"));
		assert.match(error.description, /继续对话/);
	}
});

test("详情遮蔽密钥，不将技术文本放主提示", () => {
	const e = presentError("500 apiKey=private-placeholder-token Authorization: Bearer example-secret");
	assert.ok(!e.title.includes("500"));
	assert.ok(!e.detail.includes("private-placeholder-token"));
	assert.ok(!e.detail.includes("example-secret"));
});
