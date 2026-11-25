import { X } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useDialogFocusTrap } from "../shell/dialog-focus";
import { readDurationMs, readEasing } from "../shell/motion-tokens";
import { useWorkspaceShell } from "../shell/workspace-shell-context";

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
	const trigger = useRef<HTMLButtonElement>(null);
	const image = useRef<HTMLImageElement>(null);
	const [opened, setOpened] = useState(false);
	const shell = useWorkspaceShell();
	const appActiveRef = useRef(Boolean(shell?.appActive));
	appActiveRef.current = Boolean(shell?.appActive);
	const close = useCallback(() => {
		setOpened(false);
		if (!appActiveRef.current) {
			requestAnimationFrame(() => {
				if (trigger.current?.isConnected && !trigger.current.closest("[inert]")) {
					trigger.current.focus({ preventScroll: true });
				}
			});
		}
	}, []);
	return (
		<>
			<button
				ref={trigger}
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
	const requestClose = useRef<(immediate?: boolean) => void>(() => {});
	const shell = useWorkspaceShell();
	useDialogFocusTrap(dialog);
	useEffect(() => {
		if (shell?.appActive) requestClose.current(true);
	}, [shell?.appActive]);
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
		const transform = () => {
			const source = image.getBoundingClientRect();
			const target = content.getBoundingClientRect();
			if (!image.isConnected || !source.width || !source.height || !target.width || !target.height) return "none";
			return `translate(${source.left + source.width / 2 - target.left - target.width / 2}px, ${source.top + source.height / 2 - target.top - target.height / 2}px) scale(${source.width / target.width}, ${source.height / target.height})`;
		};
		const options = (token: string, fallbackMs: number) => ({
			duration: readDurationMs(modal, token, fallbackMs),
			easing: readEasing(modal),
		});
		modal.showModal();
		document.documentElement.style.overflow = "hidden";
		if (!motion.matches) {
			animation = content.animate([{ transform: transform() }, { transform: "none" }], options("--normal", 240));
			fade = background.animate([{ opacity: 0 }, { opacity: 1 }], options("--normal", 240));
		}
		image.style.visibility = "hidden";
		const close = (immediate = false) => {
			if (closing && !immediate) return;
			closing = true;
			const current = getComputedStyle(content).transform;
			const opacity = getComputedStyle(background).opacity;
			animation?.cancel();
			fade?.cancel();
			if (immediate || motion.matches) {
				if (modal.open) modal.close();
				return;
			}
			animation = content.animate([{ transform: current }, { transform: transform() }], options("--fast", 150));
			fade = background.animate([{ opacity }, { opacity: 0 }], options("--fast", 150));
			void animation.finished.then(() => modal.close()).catch(() => {});
		};
		requestClose.current = close;
		const reduceMotion = () => {
			if (!motion.matches) return;
			animation?.cancel();
			fade?.cancel();
			if (closing && modal.open) modal.close();
		};
		// 视口改变时收回预览，避免原图坐标与放大图布局错位。
		const onResize = () => close(false);
		window.addEventListener("resize", onResize);
		motion.addEventListener("change", reduceMotion);
		return () => {
			animation?.cancel();
			fade?.cancel();
			window.removeEventListener("resize", onResize);
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
				requestClose.current(false);
			}}
			onClick={(event) => {
				event.stopPropagation();
				if (event.target === event.currentTarget) requestClose.current(false);
			}}
		>
			<div ref={scrim} className="tier-3-scrim image-preview-scrim" aria-hidden="true" />
			<button
				type="button"
				className="image-preview-close"
				aria-label="关闭图片预览"
				onClick={() => requestClose.current(false)}
			>
				<X size={22} aria-hidden="true" />
			</button>
			<button
				ref={zoom}
				type="button"
				className="image-preview-zoom"
				aria-label="收起图片"
				onClick={() => requestClose.current(false)}
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
