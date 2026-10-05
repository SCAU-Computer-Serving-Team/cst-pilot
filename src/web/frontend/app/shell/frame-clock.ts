/** 以时间戳推进背景，不按帧数累计；暂停后恢复不补播隐藏期间的时间。 */
export class FrameClock {
	private previous: number;
	private elapsed = 0;
	constructor(now: number) {
		this.previous = now;
	}
	resume(now: number) {
		this.previous = now;
	}
	step(now: number, reduced = false): number {
		if (!reduced) this.elapsed += Math.max(0, now - this.previous);
		this.previous = Math.max(this.previous, now);
		return this.elapsed / 1000;
	}
}
