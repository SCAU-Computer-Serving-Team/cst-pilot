import { useEffect, useEffectEvent, useRef, useState } from "react";
import type { ExportSnapshot } from "../data/types";

type Layer = { id: number; snapshot: ExportSnapshot; ready: boolean; animate: boolean };

function PreviewFrame({
	layer,
	scale,
	current,
	onReady,
	onError,
	onFinish,
}: {
	layer: Layer;
	scale: number;
	current: boolean;
	onReady: (id: number) => void;
	onError: (id: number) => void;
	onFinish: (id: number) => void;
}) {
	const frame = useRef<HTMLIFrameElement>(null);
	const ready = useEffectEvent(() => onReady(layer.id));
	const failed = useEffectEvent(() => onError(layer.id));
	useEffect(() => {
		const receive = (event: MessageEvent) => {
			if (event.origin !== location.origin || event.source !== frame.current?.contentWindow) return;
			if (event.data?.type === "cst-export-frame-ready")
				frame.current?.contentWindow?.postMessage(
					{ type: "cst-export-preview", snapshot: layer.snapshot },
					location.origin,
				);
			if (event.data?.type === "cst-export-content-ready") ready();
			if (event.data?.type === "cst-export-content-failed") failed();
		};
		window.addEventListener("message", receive);
		return () => window.removeEventListener("message", receive);
	}, [layer.snapshot]);
	return (
		<iframe
			ref={frame}
			src="/_export?preview=1"
			title="会话导出预览"
			width="1240"
			height="900"
			data-ready={layer.ready}
			className={layer.animate ? "export-preview-reveal" : undefined}
			aria-hidden={!current}
			inert={!current}
			style={{ transform: `scale(${scale})` }}
			onAnimationEnd={(event) => {
				if (event.target === event.currentTarget) onFinish(layer.id);
			}}
			onError={() => onError(layer.id)}
		/>
	);
}

/** 已呈现的文档保持不动，新文档完成字体与附图加载后覆盖淡入。 */
export function ExportPreview({
	snapshot,
	pending,
	onError,
}: {
	snapshot?: ExportSnapshot;
	pending: boolean;
	onError: () => void;
}) {
	const [layers, setLayers] = useState<Layer[]>([]);
	const [scale, setScale] = useState(1);
	const viewport = useRef<HTMLDivElement>(null);
	const sequence = useRef(0);
	useEffect(() => {
		const container = viewport.current;
		if (!container) return;
		const observer = new ResizeObserver(() => setScale(Math.min(1, container.clientWidth / 1240)));
		observer.observe(container);
		return () => observer.disconnect();
	}, []);
	useEffect(() => {
		const layer =
			snapshot && !pending ? { id: ++sequence.current, snapshot, ready: false, animate: false } : undefined;
		setLayers((previous) => [...previous.filter((item) => item.ready), ...(layer ? [layer] : [])]);
	}, [snapshot, pending]);
	useEffect(() => {
		const media = matchMedia("(prefers-reduced-motion: reduce)");
		const finish = () => {
			if (media.matches)
				setLayers((previous) => {
					const last = previous.filter((item) => item.ready).at(-1);
					return previous
						.filter((item) => !item.ready || item === last)
						.map((item) => ({ ...item, animate: false }));
				});
		};
		media.addEventListener("change", finish);
		return () => media.removeEventListener("change", finish);
	}, []);
	const current = layers.filter((layer) => layer.ready).at(-1);
	function ready(id: number) {
		const frames = viewport.current?.querySelectorAll("iframe");
		const previous = current ? layers.indexOf(current) : -1;
		const incoming = layers.findIndex((layer) => layer.id === id);
		if (incoming < 0 || layers[incoming].ready) return;
		if (previous >= 0 && incoming > previous) {
			const oldWindow = frames?.[previous]?.contentWindow;
			frames?.[incoming]?.contentWindow?.scrollTo(oldWindow?.scrollX ?? 0, oldWindow?.scrollY ?? 0);
		}
		const animate = previous >= 0 && !matchMedia("(prefers-reduced-motion: reduce)").matches;
		setLayers((items) =>
			items
				.filter((item) => animate || item.id >= id)
				.map((item) => (item.id === id ? { ...item, ready: true, animate } : item)),
		);
	}
	return (
		<div
			ref={viewport}
			className="export-preview-viewport"
			style={{ height: 900 * scale, visibility: current ? "visible" : "hidden" }}
		>
			{layers.map((layer) => (
				<PreviewFrame
					key={layer.id}
					layer={layer}
					scale={scale}
					current={current === layer}
					onReady={ready}
					onError={(id) => {
						setLayers((items) => items.filter((item) => item.id !== id));
						onError();
					}}
					onFinish={(id) =>
						setLayers((items) =>
							items
								.filter((item) => item.id >= id)
								.map((item) => (item.id === id ? { ...item, animate: false } : item)),
						)
					}
				/>
			))}
		</div>
	);
}
