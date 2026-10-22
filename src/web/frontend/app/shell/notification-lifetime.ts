type Clock = {
	now: () => number;
	set: (callback: () => void, delay: number) => ReturnType<typeof setTimeout>;
	clear: (timer: ReturnType<typeof setTimeout>) => void;
};
const clock: Clock = {
	now: () => performance.now(),
	set: (callback, delay) => setTimeout(callback, delay),
	clear: (timer) => clearTimeout(timer),
};

/** Each pause source owns its hold; resuming one cannot release another. */
export class NotificationLifetime {
	private remaining: number;
	private started = 0;
	private timer?: ReturnType<typeof setTimeout>;
	private readonly holds = new Set<string>();
	private ended = false;
	private readonly onElapsed: () => void;
	private readonly time: Clock;
	constructor(duration: number, onElapsed: () => void, time: Clock = clock) {
		this.remaining = duration;
		this.onElapsed = onElapsed;
		this.time = time;
	}
	resume(reason: string): void {
		this.holds.delete(reason);
		if (this.ended || this.holds.size || this.timer !== undefined) return;
		this.started = this.time.now();
		this.timer = this.time.set(() => {
			this.timer = undefined;
			this.ended = true;
			this.onElapsed();
		}, this.remaining);
	}
	pause(reason: string): void {
		this.holds.add(reason);
		if (this.timer === undefined) return;
		this.remaining = Math.max(0, this.remaining - (this.time.now() - this.started));
		this.time.clear(this.timer);
		this.timer = undefined;
	}
	dispose(): void {
		this.ended = true;
		if (this.timer !== undefined) this.time.clear(this.timer);
	}
}
