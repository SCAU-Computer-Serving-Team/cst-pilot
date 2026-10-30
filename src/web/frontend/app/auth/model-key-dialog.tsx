import { KeyRound, X } from "lucide-react";
import { useEffect, useId, useRef } from "react";
import { createPortal } from "react-dom";
import { useLocation, useNavigate } from "react-router";
import type { ModelChoice } from "../data/model-selection";

export function ModelKeyDialog({
	model,
	unsent = false,
	onClose,
}: {
	model: ModelChoice;
	unsent?: boolean;
	onClose: () => void;
}) {
	const dialog = useRef<HTMLDialogElement>(null);
	const titleId = useId();
	const descriptionId = useId();
	const navigate = useNavigate();
	const location = useLocation();
	const providerName = model.providerName ?? model.provider;
	useEffect(() => {
		const previous = document.activeElement;
		const node = dialog.current;
		node?.showModal();
		return () => {
			node?.close();
			if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
		};
	}, []);
	function configure() {
		const params = new URLSearchParams({
			provider: model.provider,
			from: "model-key",
			returnTo: `${location.pathname}${location.search}`,
		});
		onClose();
		void navigate(`/settings/provider?${params}`);
	}
	return createPortal(
		<dialog
			className="exit-dialog model-key-dialog"
			ref={dialog}
			aria-labelledby={titleId}
			aria-describedby={descriptionId}
			onCancel={(event) => {
				event.preventDefault();
				onClose();
			}}
		>
			<header className="model-key-heading">
				<KeyRound size={20} aria-hidden="true" />
				<h2 id={titleId}>配置 {providerName} 密钥</h2>
				<button type="button" className="model-key-close" aria-label="关闭密钥提示" onClick={onClose}>
					<X size={18} />
				</button>
			</header>
			<div className="model-key-identity">
				<strong>{model.name}</strong>
				<span>
					{providerName} · {model.id}
				</span>
			</div>
			<p id={descriptionId}>
				{unsent ? "尚未配置 API key。消息未发送，草稿已保留。" : "尚未配置 API key，配置后即可开始对话。"}
			</p>
			<div className="exit-actions">
				<button type="button" onClick={onClose}>
					暂不配置
				</button>
				<button type="button" className="model-key-configure" onClick={configure}>
					配置密钥
				</button>
			</div>
		</dialog>,
		document.body,
	);
}
