import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { glyphLightFrames, lightGlyphs } from "./running-light-timing";

function useLight(active: boolean) {
	const ref = useRef<HTMLSpanElement>(null);
	const [visible, setVisible] = useState(true);
	useEffect(() => {
		if (!active) return;
		let intersecting = true;
		const update = () => setVisible(intersecting && !document.hidden);
		const observer = new IntersectionObserver(([entry]) => {
			intersecting = entry.isIntersecting;
			update();
		});
		if (ref.current) observer.observe(ref.current);
		document.addEventListener("visibilitychange", update);
		update();
		return () => {
			observer.disconnect();
			document.removeEventListener("visibilitychange", update);
		};
	}, [active]);
	return { ref, "data-running": active || undefined, "data-paused": !visible || undefined };
}

/** 每个字素独立改变颜色；状态数字变化时沿用当前扫光进度。 */
export function RunningLight({ active, children }: { active: boolean; children: string }) {
	const light = useLight(active);
	const glyphs = lightGlyphs(children);
	const count = glyphs.length;
	const animations = useRef<Animation[]>([]);
	const phase = useRef(0);
	useLayoutEffect(() => {
		const element = light.ref.current;
		if (!element || !active || !count) return;
		const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
		const token = getComputedStyle(element).getPropertyValue("--running-light-duration").trim();
		const value = Number.parseFloat(token) * (token.endsWith("ms") ? 1 : 1000);
		const duration = Number.isFinite(value) ? value : 2000;
		const stop = () => {
			const time = animations.current[0]?.currentTime;
			if (typeof time === "number") phase.current = time % duration;
			for (const animation of animations.current) animation.cancel();
			animations.current = [];
		};
		const start = () => {
			stop();
			if (motion.matches) return;
			animations.current = Array.from(element.querySelectorAll<HTMLElement>(".running-glyph"), (glyph, index) => {
				const animation = glyph.animate(glyphLightFrames(index, count), {
					duration,
					iterations: Infinity,
					easing: "linear",
				});
				animation.currentTime = phase.current;
				if (element.dataset.paused) animation.pause();
				return animation;
			});
		};
		start();
		motion.addEventListener("change", start);
		return () => {
			stop();
			motion.removeEventListener("change", start);
		};
	}, [active, count, light.ref]);
	useEffect(() => {
		for (const animation of animations.current) {
			if (light["data-paused"]) animation.pause();
			else animation.play();
		}
	}, [light["data-paused"]]);
	return (
		<span className="running-light" {...light}>
			{glyphs.map((glyph, index) => (
				// biome-ignore lint/suspicious/noArrayIndexKey: 字位固定，状态数字更新保持原字位节点和光照进度。
				<span className="running-glyph" key={index}>
					{glyph}
				</span>
			))}
		</span>
	);
}

/** 特殊组件在右下角显示局部扫光，不覆盖正文与交互控件。 */
export function RunningCorner({ active }: { active: boolean }) {
	return <span className="running-corner" {...useLight(active)} aria-hidden="true" />;
}
