import type { PacerClock } from "./stream-player";

export const STREAM_STALE_MS = 45_000;
/** 依据连接心跳判断断流，工具静默或等待输入不代表连接失效。 */
export class StreamHealth {
	private timer?: unknown;
	private generation = 0;
	private readonly stale: () => void;
	private readonly clock: PacerClock;
	constructor(stale: () => void, clock?: PacerClock) {
		this.stale = stale;
		this.clock = clock ?? {
			setTimeout: (callback, ms) => window.setTimeout(callback, ms),
			clearTimeout: (handle) => window.clearTimeout(handle as number),
		};
	}
	pulse(): void {
		this.stop();
		const generation = this.generation;
		this.timer = this.clock.setTimeout(() => {
			if (generation !== this.generation) return;
			this.timer = undefined;
			this.stale();
		}, STREAM_STALE_MS);
	}
	stop(): void {
		this.generation++;
		if (this.timer !== undefined) this.clock.clearTimeout(this.timer);
		this.timer = undefined;
	}
}
