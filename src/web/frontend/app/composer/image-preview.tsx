import { useEffect, useRef } from "react";

/** 待发送图片的大图预览：原生 dialog 模态，Esc 与点背景都关闭。 */
export function ImagePreview({ src, onClose }: { src: string; onClose: () => void }) {
	const dialog = useRef<HTMLDialogElement>(null);
	useEffect(() => {
		dialog.current?.showModal();
	}, []);
	return (
		// biome-ignore lint/a11y/useKeyWithClickEvents: 键盘关闭由 dialog 原生 Escape 触发 onClose，这里的点击只处理鼠标点背景关闭
		<dialog
			ref={dialog}
			className="image-preview"
			onClose={onClose}
			onClick={(event) => {
				if (event.target === dialog.current) dialog.current?.close();
			}}
		>
			<button type="button" aria-label="关闭图片预览" onClick={() => dialog.current?.close()}>
				关闭
			</button>
			<img src={src} alt="待发送图片的大图预览" />
		</dialog>
	);
}
