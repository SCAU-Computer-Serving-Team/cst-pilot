import { TriangleAlert } from "lucide-react";
import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { restorePreviousFocus, useAnimatedDialogClose, useStandbyDialogClose } from "../shell/app-dialogs";
import { useDialogFocusTrap } from "../shell/dialog-focus";

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
	const { requestClose, closing } = useAnimatedDialogClose(dialog);
	useDialogFocusTrap(dialog, cancel);
	useStandbyDialogClose(dialog, onCancel);
	useEffect(() => {
		const previous = document.activeElement;
		const element = dialog.current;
		element?.showModal();
		cancel.current?.focus();
		return () => restorePreviousFocus(element, previous);
	}, []);
	return createPortal(
		<dialog
			ref={dialog}
			className="tier-3-dialog exit-dialog"
			aria-labelledby="credential-dialog-title"
			aria-describedby="credential-dialog-description"
			onCancel={(event) => {
				event.preventDefault();
				if (!busy && !closing) requestClose(onCancel);
			}}
		>
			<div className="dialog-title-row dialog-title-row--danger">
				<TriangleAlert size={16} aria-hidden="true" />
				<h2 id="credential-dialog-title">移除 {name} 的凭据？</h2>
			</div>
			<p id="credential-dialog-description" className="exit-description">
				移除后，此服务的模型将无法继续使用。其他服务保持不变。
			</p>
			<div className="exit-actions">
				<button
					type="button"
					ref={cancel}
					className="tier-1-btn-outline"
					disabled={busy || closing}
					onClick={() => requestClose(onCancel)}
				>
					取消
				</button>
				<button
					type="button"
					className="tier-1-btn-danger solid-danger exit-confirm"
					disabled={busy || closing}
					onClick={onConfirm}
				>
					{busy ? "正在移除…" : "移除凭据"}
				</button>
			</div>
		</dialog>,
		document.body,
	);
}
