import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message } from "./web-state.ts";
import { pacingStep, playableText, sliceMessage, snapBudget, StreamPacer, type PacerClock } from "./stream-player.ts";

test("pacingStep 按积压平滑缩放并封顶：慢模型 1 字、快模型 ≤12 字", () => {
	assert.equal(pacingStep(1), 1);
	assert.equal(pacingStep(32), 1);
	assert.equal(pacingStep(33), 2);
	assert.equal(pacingStep(64), 2);
	assert.equal(pacingStep(200), 7);
	assert.equal(pacingStep(384), 12);
	assert.equal(pacingStep(4000), 12);
});

const msg = (content: Message["content"], timestamp = 1): Message => ({ role: "assistant", content, timestamp });

test("playableText 串联 text 与 thinking，跳过 toolCall", () => {
	assert.equal(playableText(msg("字符串内容")), "字符串内容");
	assert.equal(playableText(msg([
		{ type: "thinking", thinking: "想" },
		{ type: "text", text: "正文" },
		{ type: "toolCall", id: "a", name: "disk", arguments: {} },
		{ type: "text", text: "结论" },
	])), "想正文结论");
});

test("sliceMessage 按预算裁剪，跨 part 边界与 toolCall 整体出现", () => {
	const message = msg([
		{ type: "thinking", thinking: "思考" },
		{ type: "text", text: "你好世界" },
		{ type: "toolCall", id: "a", name: "disk", arguments: {} },
	]);
	// 预算覆盖全部可播字符：toolCall 完整保留
	const full = sliceMessage(message, 6);
	assert.equal(full.content.length, 3);
	// 预算落在 text 中间：截断且丢弃后续 toolCall
	const cut = sliceMessage(message, 4);
	assert.equal(cut.content.length, 2);
	assert.deepEqual(cut.content[1], { type: "text", text: "你好" });
	// 预算为零：只有整体出现的语义不剩任何可播内容
	assert.deepEqual(sliceMessage(message, 0).content, []);
	// 字符串 content 直接截断
	assert.equal(sliceMessage(msg("abcdef"), 3).content, "abc");
});

test("snapBudget 向后吸附到窗口内最后的词边界", () => {
	assert.equal(snapBudget("hello world 你好", 5), 12);
	assert.equal(snapBudget("hello,world", 4), 6);
	assert.equal(snapBudget("abcdefgh", 3), 3);
	assert.equal(snapBudget("ab cd", 10), 10);
});

/** 手动时钟：收集回调，测试按需触发。 */
function manualClock(): { clock: PacerClock; fire: () => void; pendingCount: () => number } {
	const queue: (() => void)[] = [];
	return {
		clock: { setTimeout: (callback) => queue.push(callback), clearTimeout: () => queue.pop() },
		fire: () => queue.shift()?.(),
		pendingCount: () => queue.length,
	};
}

test("Pacer：首个快照立即显示第一步，后续每 24ms 步进", () => {
	const { clock, fire, pendingCount } = manualClock();
	const published: Message[] = [];
	const pacer = new StreamPacer((m) => published.push(m), clock);
	pacer.update(msg("短消息"));
	// 立即走一步：小积压步长 1 字（逐字），且排了下一个定时器
	assert.equal(published.length, 1);
	assert.equal(published[0]!.content, "短");
	assert.equal(pendingCount(), 1);
	fire();
	assert.equal(published.at(-1)!.content, "短消");
	fire();
	assert.equal(published.at(-1)!.content, "短消息");
	assert.equal(pendingCount(), 0);
});

test("Pacer：突发积压按步长分档追赶，追平后停表", () => {
	const { clock, fire, pendingCount } = manualClock();
	const published: Message[] = [];
	const pacer = new StreamPacer((m) => published.push(m), clock);
	const text = "x".repeat(600);
	pacer.update(msg(text));
	// 首步立即显示（600 字积压 → 步长封顶 12 字），不再一次性放出大段
	assert.equal(typeof published[0]!.content === "string" ? published[0]!.content.length : 0, 12);
	// 逐步追平：步长随剩余量递减，末段小步收尾
	let steps = 0;
	while (pendingCount() > 0 && steps < 200) {
		fire();
		steps++;
	}
	const last = published.at(-1)!;
	assert.equal(typeof last.content === "string" ? last.content.length : 0, text.length);
	assert.equal(pendingCount(), 0);
	// 追平后同内容再更新：无新内容不排定时器
	pacer.update(msg(text));
	assert.equal(published.length, steps + 1);
});

test("Pacer：flush 停止追赶，reset 清空后新消息从头播放", () => {
	const { clock, fire, pendingCount } = manualClock();
	const published: Message[] = [];
	const pacer = new StreamPacer((m) => published.push(m), clock);
	pacer.update(msg("y".repeat(600), 100));
	pacer.flush();
	assert.equal(pendingCount(), 0);
	pacer.reset();
	// 新消息（timestamp 不同）重置进度后从零追赶
	pacer.update(msg("z".repeat(600), 200));
	fire();
	const first = published[0];
	assert.ok(typeof first!.content === "string" && first!.content.length < 600);
});
