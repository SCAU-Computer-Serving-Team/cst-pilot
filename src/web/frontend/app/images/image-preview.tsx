import { X } from "lucide-react";
import { useCallback, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

/** 所有本地图片共用预览入口；仅在打开时挂载模态与动效监听。 */
export function PreviewableImage({
	src,
	alt,
	className,
	buttonClassName = "image-open",
	label = `预览${alt}`,
}: {
	src: string;
	alt: string;
	className?: string;
	buttonClassName?: string;
	label?: string;
}) {
	const image = useRef<HTMLImageElement>(null);
	const [opened, setOpened] = useState(false);
	const close = useCallback(() => setOpened(false), []);
	return (
		<>
			<button
				type="button"
				className={buttonClassName}
				aria-label={label}
				onClick={() => {
					if (image.current?.naturalWidth) setOpened(true);
				}}
			>
				<img ref={image} src={src} alt={alt} className={className} />
			</button>
			{opened &&
				image.current &&
				createPortal(<ImagePreview image={image.current} onClose={close} />, document.body)}
		</>
	);
}

function ImagePreview({ image, onClose }: { image: HTMLImageElement; onClose: () => void }) {
	const dialog = useRef<HTMLDialogElement>(null);
	const zoom = useRef<HTMLButtonElement>(null);
	const scrim = useRef<HTMLDivElement>(null);
	const requestClose = useRef(() => {});
	useLayoutEffect(() => {
		const modal = dialog.current;
		const content = zoom.current;
		const background = scrim.current;
		if (!modal || !content || !background) return;
		const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
		const visibility = image.style.visibility;
		const overflow = document.documentElement.style.overflow;
		let animation: Animation | undefined;
		let fade: Animation | undefined;
		let closing = false;
		const duration = (token: string) => {
			const value = getComputedStyle(modal).getPropertyValue(token).trim();
			return Number.parseFloat(value) * (value.endsWith("ms") ? 1 : 1000);
		};
		const transform = () => {
			const source = image.getBoundingClientRect();
			const target = content.getBoundingClientRect();
			if (!image.isConnected || !source.width || !source.height || !target.width || !target.height) return "none";
			return `translate(${source.left + source.width / 2 - target.left - target.width / 2}px, ${source.top + source.height / 2 - target.top - target.height / 2}px) scale(${source.width / target.width}, ${source.height / target.height})`;
		};
		const options = (token: string) => ({
			duration: duration(token),
			easing: getComputedStyle(modal).getPropertyValue("--ease-smooth-out").trim(),
		});
		modal.showModal();
		document.documentElement.style.overflow = "hidden";
		if (!motion.matches) {
			animation = content.animate([{ transform: transform() }, { transform: "none" }], options("--duration-medium"));
			fade = background.animate([{ opacity: 0 }, { opacity: 1 }], options("--duration-medium"));
		}
		image.style.visibility = "hidden";
		const close = () => {
			if (closing) return;
			closing = true;
			const current = getComputedStyle(content).transform;
			const opacity = getComputedStyle(background).opacity;
			animation?.cancel();
			fade?.cancel();
			if (motion.matches) {
				modal.close();
				return;
			}
			animation = content.animate([{ transform: current }, { transform: transform() }], options("--duration-fast"));
			fade = background.animate([{ opacity }, { opacity: 0 }], options("--duration-fast"));
			void animation.finished.then(() => modal.close()).catch(() => {});
		};
		requestClose.current = close;
		const reduceMotion = () => {
			if (!motion.matches) return;
			animation?.cancel();
			fade?.cancel();
			if (closing) modal.close();
		};
		// 视口改变时收回预览，避免原图坐标与放大图布局错位。
		window.addEventListener("resize", close);
		motion.addEventListener("change", reduceMotion);
		return () => {
			animation?.cancel();
			fade?.cancel();
			window.removeEventListener("resize", close);
			motion.removeEventListener("change", reduceMotion);
			image.style.visibility = visibility;
			document.documentElement.style.overflow = overflow;
			if (modal.open) modal.close();
		};
	}, [image]);
	return (
		// biome-ignore lint/a11y/useKeyWithClickEvents: dialog 原生 Esc 与内部关闭按钮提供键盘操作，背景点击仅处理鼠标。
		<dialog
			ref={dialog}
			className="image-preview"
			aria-label="图片预览"
			onClose={onClose}
			onCancel={(event) => {
				event.preventDefault();
				requestClose.current();
			}}
			onClick={(event) => {
				event.stopPropagation();
				if (event.target === event.currentTarget) requestClose.current();
			}}
		>
			<div ref={scrim} className="image-preview-scrim" aria-hidden="true" />
			<button
				type="button"
				className="image-preview-close"
				aria-label="关闭图片预览"
				onClick={() => requestClose.current()}
			>
				<X size={22} aria-hidden="true" />
			</button>
			<button
				ref={zoom}
				type="button"
				className="image-preview-zoom"
				aria-label="收起图片"
				onClick={() => requestClose.current()}
			>
				<img
					src={image.currentSrc || image.src}
					alt={image.alt}
					width={image.naturalWidth}
					height={image.naturalHeight}
					draggable={false}
				/>
			</button>
		</dialog>
	);
}
