import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { glyphLightFrames, lightGlyphs } from "./running-light-timing";

const ASCII_DIGIT = /^[0-9]$/;

type DigitRun = { start: number; end: number };
type DigitSlotMeta = { slotKey: string; digit: string };
type DigitTransition = { prev: string; next: string; tick: number };

function findDigitRuns(glyphs: readonly string[]): DigitRun[] {
	const runs: DigitRun[] = [];
	let i = 0;
	while (i < glyphs.length) {
		if (!ASCII_DIGIT.test(glyphs[i])) {
			i++;
			continue;
		}
		const start = i;
		while (i < glyphs.length && ASCII_DIGIT.test(glyphs[i])) i++;
		runs.push({ start, end: i });
	}
	return runs;
}

/** 将字素列表映射为稳定的结构键：数字按「段序号 + 右起十进制位」编号，避免 9s → 10s 时复用错位。 */
function buildGlyphTokens(glyphs: readonly string[]): Array<{ key: string; glyph: string; digitSlot?: DigitSlotMeta }> {
	const runs = findDigitRuns(glyphs);
	const out: Array<{ key: string; glyph: string; digitSlot?: DigitSlotMeta }> = [];
	let runCursor = 0;
	let staticCursor = 0;
	let i = 0;
	while (i < glyphs.length) {
		const run = runs[runCursor];
		if (run && i === run.start) {
			const len = run.end - run.start;
			for (let pos = 0; pos < len; pos++) {
				const place = len - 1 - pos;
				const slotKey = `d-${runCursor}-p-${place}`;
				const glyph = glyphs[run.start + pos];
				out.push({ key: slotKey, glyph, digitSlot: { slotKey, digit: glyph } });
			}
			i = run.end;
			runCursor++;
		} else {
			out.push({ key: `t-${runCursor}-${staticCursor}`, glyph: glyphs[i] });
			staticCursor++;
			i++;
		}
	}
	return out;
}

function collectDigitMap(glyphs: readonly string[]): Map<string, string> {
	const map = new Map<string, string>();
	const runs = findDigitRuns(glyphs);
	for (let r = 0; r < runs.length; r++) {
		const run = runs[r];
		const len = run.end - run.start;
		for (let pos = 0; pos < len; pos++) {
			const place = len - 1 - pos;
			map.set(`d-${r}-p-${place}`, glyphs[run.start + pos]);
		}
	}
	return map;
}

function prefersReducedMotion(): boolean {
	return typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

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

/** 每个字素独立改变颜色；状态数字变化时沿用当前扫光进度，仅运行可见态下为真实变动的数字位配对起播 ±6px 滚动。 */
export function RunningLight({ active, children }: { active: boolean; children: string }) {
	const light = useLight(active);
	const glyphs = lightGlyphs(children);
	const tokens = buildGlyphTokens(glyphs);
	const count = glyphs.length;
	const animations = useRef<Animation[]>([]);
	const phase = useRef(0);
	const committedTextRef = useRef(children);
	const tickCounterRef = useRef(0);
	const [reducedMotion, setReducedMotion] = useState(prefersReducedMotion);
	const [digitTransitions, setDigitTransitions] = useState<Record<string, DigitTransition>>({});
	const paused = Boolean(light["data-paused"]);
	const canTick = active && !paused && !reducedMotion;

	useEffect(() => {
		if (typeof window === "undefined") return;
		const media = window.matchMedia("(prefers-reduced-motion: reduce)");
		const sync = () => setReducedMotion(media.matches);
		sync();
		media.addEventListener("change", sync);
		return () => media.removeEventListener("change", sync);
	}, []);

	// 仅在提交后的 layout 阶段比对基线，确保被放弃的渲染不污染比较基线；暂停或减少动态时消费当前值并清空动画。
	useLayoutEffect(() => {
		const prevText = committedTextRef.current;
		committedTextRef.current = children;
		if (!canTick) {
			setDigitTransitions((prev) => (Object.keys(prev).length ? {} : prev));
			return;
		}
		if (prevText === children) return;
		const prevMap = collectDigitMap(lightGlyphs(prevText));
		const nextMap = collectDigitMap(lightGlyphs(children));
		const nextTransitions: Record<string, DigitTransition> = {};
		for (const [slotKey, nextDigit] of nextMap) {
			const prevDigit = prevMap.get(slotKey);
			if (prevDigit !== nextDigit) {
				tickCounterRef.current += 1;
				nextTransitions[slotKey] = { prev: prevDigit ?? "", next: nextDigit, tick: tickCounterRef.current };
			}
		}
		setDigitTransitions(nextTransitions);
	}, [children, canTick]);

	useLayoutEffect(() => {
		const element = light.ref.current;
		if (!element || !active || !count) return;
		const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
		const token = getComputedStyle(element).getPropertyValue("--running-light-duration").trim();
		const value = Number.parseFloat(token) * (token.endsWith("ms") ? 1 : 1000);
		const duration = Number.isFinite(value) && value > 0 ? value : 2000;
		const stop = () => {
			const time = animations.current[0]?.currentTime;
			if (typeof time === "number" && Number.isFinite(time)) phase.current = time % duration;
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
			{tokens.map((item) => {
				const transition = canTick && item.digitSlot ? digitTransitions[item.digitSlot.slotKey] : undefined;
				return (
					<span className="running-glyph" key={item.key}>
						{transition && transition.next === item.glyph ? (
							<span className="running-digit-slot">
								{transition.prev && (
									<span className="running-digit-out" key={`out-${transition.tick}`} aria-hidden="true">
										{transition.prev}
									</span>
								)}
								<span className="running-digit-in" key={`in-${transition.tick}`}>
									{item.glyph}
								</span>
							</span>
						) : (
							item.glyph
						)}
					</span>
				);
			})}
		</span>
	);
}

/** 特殊组件在右下角显示局部扫光，不覆盖正文与交互控件。 */
export function RunningCorner({ active }: { active: boolean }) {
	return <span className="running-corner" {...useLight(active)} aria-hidden="true" />;
}
