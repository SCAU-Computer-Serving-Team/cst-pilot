import { useEffect, useLayoutEffect, useRef } from "react";

/** 仅在条目数量或展开状态变化时衔接高度，完成后恢复内容自适应。 */
export function useAnimatedHeight(revision: string | number) {
	const root = useRef<HTMLDivElement>(null);
	const height = useRef<number | null>(null);
	const animation = useRef<Animation | null>(null);
	const observed = useRef<HTMLDivElement | null>(null);
	const observer = useRef<ResizeObserver | null>(null);
	// biome-ignore lint/correctness/useExhaustiveDependencies: revision 只用于触发 DOM 更新后的尺寸采样。
	useLayoutEffect(() => {
		const node = root.current;
		if (!node) return;
		if (observed.current !== node) {
			observer.current?.disconnect();
			observed.current = node;
			height.current = null;
			observer.current = new ResizeObserver(() => {
				if (!animation.current) height.current = node.getBoundingClientRect().height;
			});
			observer.current.observe(node);
		}
		const from = animation.current ? node.getBoundingClientRect().height : height.current;
		animation.current?.cancel();
		animation.current = null;
		const to = node.getBoundingClientRect().height;
		height.current = to;
		if (from === null || Math.abs(from - to) < 1 || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
		const style = getComputedStyle(node);
		const time = style.getPropertyValue("--duration-fast").trim();
		const duration = Number.parseFloat(time) * (time.endsWith("ms") ? 1 : 1000);
		const next = node.animate(
			[
				{ height: `${from}px`, overflow: "clip" },
				{ height: `${to}px`, overflow: "clip" },
			],
			{
				duration,
				easing: style.getPropertyValue("--ease-smooth-out").trim(),
			},
		);
		animation.current = next;
		void next.finished
			.then(() => {
				if (animation.current === next) {
					animation.current = null;
					height.current = node.getBoundingClientRect().height;
				}
			})
			.catch(() => {});
	}, [revision]);
	useEffect(() => {
		const preference = matchMedia("(prefers-reduced-motion: reduce)");
		const cancel = () => {
			animation.current?.cancel();
			animation.current = null;
		};
		preference.addEventListener("change", cancel);
		return () => {
			cancel();
			observer.current?.disconnect();
			preference.removeEventListener("change", cancel);
		};
	}, []);
	return root;
}
