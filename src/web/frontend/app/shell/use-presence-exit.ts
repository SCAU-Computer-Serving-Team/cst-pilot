import { useEffect, useState } from "react";

const EXIT_DURATION_MS = 150;

function prefersReducedMotion(): boolean {
	return typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * 轻量退场状态原语：关闭首帧即保持同一节点挂载并切入 150ms 无交互退场，
 * 中途重新打开立即取消定时器并反向衔接；开启减少动态时立即卸载。
 */
export function usePresenceExit<T>(value: T | null | undefined, durationMs = EXIT_DURATION_MS) {
	const open = value !== null && value !== undefined && value !== false;
	const [snapshot, setSnapshot] = useState<T | null>(open ? (value as T) : null);

	if (open && value !== snapshot) {
		setSnapshot(value as T);
	}

	const canAnimateExit = durationMs > 0 && !prefersReducedMotion();
	const exiting = !open && snapshot !== null && canAnimateExit;

	useEffect(() => {
		if (open || snapshot === null) return;
		if (!canAnimateExit) {
			setSnapshot(null);
			return;
		}
		const media = window.matchMedia("(prefers-reduced-motion: reduce)");
		const finish = () => {
			setSnapshot(null);
		};
		const onMotionChange = () => {
			if (media.matches) finish();
		};
		media.addEventListener("change", onMotionChange);
		const timer = window.setTimeout(finish, durationMs);
		return () => {
			window.clearTimeout(timer);
			media.removeEventListener("change", onMotionChange);
		};
	}, [open, snapshot, durationMs, canAnimateExit]);

	return {
		open,
		exiting,
		mounted: open || exiting,
		value: open ? (value as T) : snapshot,
	};
}
