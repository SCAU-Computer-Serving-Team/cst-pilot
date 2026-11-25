import { useCallback, useLayoutEffect, useRef } from "react";
import { readDurationMs, readEasing } from "./motion-tokens";

const targets =
	".home-center, .home-footnote, .home-backdrop, .chat-composer-area, .conversation > *, .chat-header > *, .export-toolbar > *, .settings-content, .session-list-content, .tree-toolbar > *, .tree-list";

/** 布局只更新一次，随后以合成层位移衔接，避免侧栏每帧改宽触发全页重排。 */
export function useSidebarMotion(collapsed: boolean) {
	const root = useRef<HTMLDivElement>(null);
	const positions = useRef(new Map<HTMLElement, DOMRect>());
	const animations = useRef<Animation[]>([]);
	const capture = useCallback(() => {
		positions.current = new Map(
			[...(root.current?.querySelectorAll<HTMLElement>(targets) ?? [])].map((element) => [
				element,
				element.getBoundingClientRect(),
			]),
		);
		for (const animation of animations.current) animation.cancel();
		animations.current = [];
		root.current?.removeAttribute("data-sidebar-reveal");
	}, []);
	useLayoutEffect(() => {
		const previous = positions.current;
		positions.current = new Map();
		if (!previous.size || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
		const duration = readDurationMs(
			document.documentElement,
			collapsed ? "--fast" : "--normal",
			collapsed ? 150 : 240,
		);
		const easing = readEasing(document.documentElement);
		const measurements = [...previous]
			.filter(([element]) => element.isConnected)
			.map(([element, before]) => {
				const after = element.getBoundingClientRect();
				const backdrop = element.classList.contains("home-backdrop");
				return {
					element,
					delta: backdrop
						? before.left - after.left
						: before.left + before.width / 2 - after.left - after.width / 2,
					scale: backdrop && after.width > 0 ? before.width / after.width : 1,
					transform: getComputedStyle(element).transform,
				};
			});
		// 展开时背景仍在侧栏后方，允许它跨过一次性更新后的主区边界。
		if (
			!collapsed &&
			measurements.some(({ element, delta }) => element.classList.contains("home-backdrop") && delta < -0.5)
		) {
			root.current?.setAttribute("data-sidebar-reveal", "true");
		}
		for (const { element, delta, scale, transform } of measurements) {
			if ((Math.abs(delta) < 0.5 && Math.abs(scale - 1) < 0.001) || typeof element.animate !== "function") continue;
			const base = transform === "none" ? "" : transform;
			animations.current.push(
				element.animate(
					[{ transform: `translate3d(${delta}px, 0, 0) scaleX(${scale}) ${base}` }, { transform: base || "none" }],
					{
						duration,
						easing,
					},
				),
			);
		}
		const batch = animations.current;
		const owner = root.current;
		void Promise.allSettled(batch.map((animation) => animation.finished)).then(() => {
			if (animations.current === batch) owner?.removeAttribute("data-sidebar-reveal");
		});
	}, [collapsed]);
	useLayoutEffect(
		() => () => {
			for (const animation of animations.current) animation.cancel();
			root.current?.removeAttribute("data-sidebar-reveal");
		},
		[],
	);
	return { root, capture };
}
