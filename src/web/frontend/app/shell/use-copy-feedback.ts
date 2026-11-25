import { useCallback, useEffect, useRef, useState } from "react";

export type CopyStatus = "idle" | "copied" | "error";

/** 共用剪贴板复制反馈：防并发写入竞态覆盖定时器，并在卸载后停止更新状态或创建新定时器。 */
export function useCopyFeedback(options?: {
	copiedDurationMs?: number;
	errorDurationMs?: number | null;
	onCopied?: () => void;
	onError?: () => void;
}) {
	const [status, setStatus] = useState<CopyStatus>("idle");
	const mountedRef = useRef(true);
	const inFlightRef = useRef(false);
	const timerRef = useRef<number | undefined>(undefined);
	const optionsRef = useRef(options);
	optionsRef.current = options;

	useEffect(() => {
		mountedRef.current = true;
		return () => {
			mountedRef.current = false;
			window.clearTimeout(timerRef.current);
		};
	}, []);

	const copy = useCallback(async (text: string) => {
		if (inFlightRef.current) return;
		inFlightRef.current = true;
		window.clearTimeout(timerRef.current);
		try {
			await navigator.clipboard.writeText(text);
			if (!mountedRef.current) return;
			setStatus("copied");
			optionsRef.current?.onCopied?.();
			const duration = optionsRef.current?.copiedDurationMs ?? 1600;
			timerRef.current = window.setTimeout(() => {
				if (mountedRef.current) setStatus("idle");
			}, duration);
		} catch {
			if (!mountedRef.current) return;
			setStatus("error");
			optionsRef.current?.onError?.();
			const errorDuration = optionsRef.current?.errorDurationMs;
			if (errorDuration !== null) {
				timerRef.current = window.setTimeout(() => {
					if (mountedRef.current) setStatus("idle");
				}, errorDuration ?? 2400);
			}
		} finally {
			inFlightRef.current = false;
		}
	}, []);

	return { status, copied: status === "copied", copy };
}
