import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { readDurationMs } from "./motion-tokens";

const EXIT_DURATION_MS = 150;

function prefersReducedMotion(): boolean {
	return typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export type PresenceState = "entering" | "open" | "exiting";

/**
 * 轻量同节点入退场原语（兼容 Chromium 111+）：
 * - 首次挂载先以 `entering` 落地，下一帧切到 `open` 触发可反向过渡；
 * - 关闭首帧即保持同一节点并切入 `exiting`，监听真实 `transitionend` 收尾（带计算时长兜底）；
 * - 中途重开立即取消收尾并从当前进度反向回弹；开启减少动态时立即卸载。
 */
export function usePresenceExit<T>(value: T | null | undefined, durationMs = EXIT_DURATION_MS) {
	const open = value !== null && value !== undefined && value !== false;
	const [snapshot, setSnapshot] = useState<T | null>(open ? (value as T) : null);
	const [entered, setEntered] = useState(() => open && prefersReducedMotion());
	const surfaceElRef = useRef<HTMLElement | null>(null);
	const surfaceRef = useCallback((node: HTMLElement | null) => {
		surfaceElRef.current = node;
	}, []);

	if (open && value !== snapshot) {
		setSnapshot(value as T);
	}

	const reducedNow = prefersReducedMotion();
	const canAnimateExit = durationMs > 0 && !reducedNow;
	const exiting = !open && snapshot !== null && canAnimateExit;
	const mounted = open || exiting;

	useLayoutEffect(() => {
		if (!mounted) {
			setEntered(false);
			return;
		}
		if (!open) return;
		if (prefersReducedMotion()) {
			setEntered(true);
			return;
		}
		if (entered) return;
		// 强制落地初始样式后再切入 open，保证 Chromium 111–116 与当前版都能稳定播放首次入场。
		void surfaceElRef.current?.offsetWidth;
		const frame = window.requestAnimationFrame(() => setEntered(true));
		return () => window.cancelAnimationFrame(frame);
	}, [mounted, open, entered]);

	useEffect(() => {
		if (open || snapshot === null) return;
		if (!canAnimateExit) {
			setSnapshot(null);
			setEntered(false);
			return;
		}
		let finished = false;
		const finish = () => {
			if (finished) return;
			finished = true;
			setSnapshot(null);
			setEntered(false);
		};
		const media = window.matchMedia("(prefers-reduced-motion: reduce)");
		const onMotionChange = () => {
			if (media.matches) finish();
		};
		media.addEventListener("change", onMotionChange);
		if (media.matches) {
			finish();
			return () => media.removeEventListener("change", onMotionChange);
		}
		const el = surfaceElRef.current;
		const computedMs = el ? readDurationMs(el, "--fast", durationMs) : durationMs;
		const onTransitionEnd = (event: TransitionEvent) => {
			if (event.propertyName === "opacity" && (event.target === el || el?.contains(event.target as Node))) {
				finish();
			}
		};
		el?.addEventListener("transitionend", onTransitionEnd);
		const timer = window.setTimeout(finish, Math.max(0, computedMs) + 40);
		return () => {
			window.clearTimeout(timer);
			el?.removeEventListener("transitionend", onTransitionEnd);
			media.removeEventListener("change", onMotionChange);
		};
	}, [open, snapshot, durationMs, canAnimateExit]);

	const state: PresenceState = exiting ? "exiting" : entered || reducedNow ? "open" : "entering";

	return {
		open,
		exiting,
		mounted,
		state,
		surfaceRef,
		value: open ? (value as T) : snapshot,
	};
}
