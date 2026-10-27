import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";

export function CredentialDialog({
	name,
	busy,
	onCancel,
	onConfirm,
}: {
	name: string;
	busy: boolean;
	onCancel: () => void;
	onConfirm: () => void;
}) {
	const dialog = useRef<HTMLDialogElement>(null);
	const cancel = useRef<HTMLButtonElement>(null);
	useEffect(() => {
		const previous = document.activeElement;
		const element = dialog.current;
		element?.showModal();
		cancel.current?.focus();
		return () => {
			element?.close();
			if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
		};
	}, []);
	return createPortal(
		<dialog
			ref={dialog}
			className="exit-dialog"
			aria-labelledby="credential-dialog-title"
			aria-describedby="credential-dialog-description"
			onCancel={(event) => {
				event.preventDefault();
				if (!busy) onCancel();
			}}
		>
			<h2 id="credential-dialog-title">移除 {name} 的凭据？</h2>
			<p id="credential-dialog-description" className="exit-description">
				移除后，此服务的模型将无法继续使用。其他服务保持不变。
			</p>
			<div className="exit-actions">
				<button type="button" ref={cancel} disabled={busy} onClick={onCancel}>
					取消
				</button>
				<button type="button" className="exit-confirm" disabled={busy} onClick={onConfirm}>
					{busy ? "正在移除…" : "移除凭据"}
				</button>
			</div>
		</dialog>,
		document.body,
	);
}
