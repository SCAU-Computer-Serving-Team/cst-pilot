import { useEffect, useRef, useState } from "react";

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

/** 光影只覆盖状态字形，原文字始终保留。 */
export function RunningLight({ active, children }: { active: boolean; children: string }) {
	return (
		<span className="running-light" {...useLight(active)} data-text={children}>
			{children}
		</span>
	);
}

/** 特殊组件在右下角显示局部扫光，不覆盖正文与交互控件。 */
export function RunningCorner({ active }: { active: boolean }) {
	return <span className="running-corner" {...useLight(active)} aria-hidden="true" />;
}
