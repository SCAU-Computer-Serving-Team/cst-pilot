import type { Message } from "./web-state";

// 流式播放节奏（opencode desktop 的追赶式 pacing，持续步进版）：
// 流式期间每 24ms 步进一批，步长随积压分档（匀速供给→匀速播放，积压大→自动加速追赶）；
// 消息结束（message_end/agent_end）或重置时立即放行剩余内容。
export const PACE_MS = 24;
const SNAP_WINDOW = 8;
const SNAP = /[\s.,!?;:)\]]/;

/** 每步推进的字符数：按积压平滑缩放并小封顶——慢模型稳态 1 字（逐字），快模型稳态 8~12 字（快速打字机），
 * 吞吐上限约 500 字/秒，覆盖 200 token/s 峰值，同时避免任何情况下一次跳出一大串。 */
export function pacingStep(remaining: number): number {
	return Math.max(1, Math.min(12, Math.ceil(remaining / 32)));
}

/** 参与播放的字符总量：text 与 thinking 按内容顺序计数；toolCall/图片整体出现不占预算。 */
export function playableText(message: Message): string {
	if (typeof message.content === "string") return message.content;
	let out = "";
	for (const part of message.content) {
		if (part.type === "text") out += part.text;
		else if (part.type === "thinking") out += part.thinking;
	}
	return out;
}

/** 按预算裁出消息前缀视图：预算用尽处丢弃其后全部内容（含 toolCall，按到达顺序播放）。 */
export function sliceMessage(message: Message, budget: number): Message {
	if (typeof message.content === "string") return { ...message, content: message.content.slice(0, budget) };
	if (budget <= 0) return { ...message, content: [] };
	const content = [];
	for (const part of message.content) {
		if (part.type === "text" || part.type === "thinking") {
			const text = part.type === "text" ? part.text : part.thinking;
			if (budget >= text.length) {
				content.push(part);
				budget -= text.length;
				continue;
			}
			if (budget > 0) content.push(part.type === "text" ? { ...part, text: text.slice(0, budget) } : { ...part, thinking: text.slice(0, budget) });
			return { ...message, content };
		}
		content.push(part);
	}
	return { ...message, content };
}

/** 步进终点在 +8 字符窗口内吸附最近的空白或标点，避免把拉丁词切成两半。 */
export function snapBudget(text: string, budget: number): number {
	const limit = Math.min(text.length, budget + SNAP_WINDOW);
	for (let i = limit - 1; i >= budget; i--) {
		if (SNAP.test(text[i]!)) return i + 1;
	}
	return budget;
}

/** 定时器注入，便于在 node:test 中驱动。 */
export interface PacerClock {
	setTimeout: (callback: () => void, ms: number) => unknown;
	clearTimeout: (handle: unknown) => void;
}

/** 追赶式播放器：消化突发积压，步长分档推进，直通阈值内立即放行。 */
export class StreamPacer {
	private shown = 0;
	private message?: Message;
	private projected = "";
	private timer?: unknown;

	private readonly publish: (message: Message) => void;
	private readonly clock: PacerClock;

	constructor(publish: (message: Message) => void, clock?: PacerClock) {
		this.publish = publish;
		this.clock = clock ?? { setTimeout: (callback, ms) => window.setTimeout(callback, ms), clearTimeout: (handle) => window.clearTimeout(handle as number) };
	}

	/** 收到新的全量快照：有未播放内容时立即走一步，并保持 24ms 步进直到追平。 */
	update(message: Message): void {
		if (this.message?.timestamp !== message.timestamp) {
			this.shown = 0;
			this.projected = "";
		}
		this.message = message;
		this.projected = playableText(message);
		if (this.projected.length > this.shown && this.timer === undefined) this.tick();
	}

	/** 消息结束/放弃追赶：停止步进（调用方随即清除流式态，全量由刷新落定）。 */
	flush(): void {
		if (this.message) this.shown = this.projected.length;
		this.stop();
	}

	reset(): void {
		this.stop();
		this.message = undefined;
		this.projected = "";
		this.shown = 0;
	}

	private schedule(): void {
		this.timer = this.clock.setTimeout(() => {
			this.timer = undefined;
			this.tick();
		}, PACE_MS);
	}
	private stop(): void {
		if (this.timer !== undefined) {
			this.clock.clearTimeout(this.timer);
			this.timer = undefined;
		}
	}
	private tick(): void {
		if (!this.message || this.shown >= this.projected.length) return;
		const next = snapBudget(this.projected, this.shown + pacingStep(this.projected.length - this.shown));
		this.shown = next;
		this.publish(sliceMessage(this.message, next));
		if (next < this.projected.length) this.schedule();
	}
}
