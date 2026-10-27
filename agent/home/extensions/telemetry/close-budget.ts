/** 关闭只等待统一时限；超时后的本地持久化继续执行，失败由原任务处理。 */
export async function withinBudget(task: Promise<unknown>, signal: AbortSignal): Promise<void> {
	if (signal.aborted) {
		void task.catch(() => undefined);
		return;
	}
	let abort!: () => void;
	const expired = new Promise<void>((resolve) => {
		abort = resolve;
		signal.addEventListener("abort", abort, { once: true });
	});
	try {
		await Promise.race([task, expired]);
	} finally {
		signal.removeEventListener("abort", abort);
	}
}
