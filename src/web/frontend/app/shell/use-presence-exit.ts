import { useEffect, useState } from "react";

const EXIT_DURATION_MS = 150;

function prefersReducedMotion(): boolean {
	return typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * 轻量退场状态原语：关闭时保留最后快照播放 150ms 无交互退场动画，
 * 中途重新打开立即取消定时器并无缝衔接；减少动态下 0ms 瞬退。
 */
export function usePresenceExit<T>(value: T | null | undefined, durationMs = EXIT_DURATION_MS) {
	const open = value !== null && value !== undefined && value !== false;
	const [snapshot, setSnapshot] = useState<T | null>(open ? (value as T) : null);
	const [exiting, setExiting] = useState(false);

	if (open && value !== snapshot) {
		setSnapshot(value as T);
	}
	if (open && exiting) {
		setExiting(false);
	}

	useEffect(() => {
		if (open) return;
		if (snapshot === null) return;
		if (durationMs <= 0 || prefersReducedMotion()) {
			setExiting(false);
			setSnapshot(null);
			return;
		}
		setExiting(true);
		const timer = window.setTimeout(() => {
			setExiting(false);
			setSnapshot(null);
		}, durationMs);
		return () => window.clearTimeout(timer);
	}, [open, snapshot, durationMs]);

	return {
		open,
		exiting: !open && exiting && snapshot !== null,
		mounted: open || (exiting && snapshot !== null),
		value: open ? (value as T) : snapshot,
	};
}
