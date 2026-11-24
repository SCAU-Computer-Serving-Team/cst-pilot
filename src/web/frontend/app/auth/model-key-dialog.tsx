import { KeyRound, X } from "lucide-react";
import { useEffect, useId, useRef } from "react";
import { createPortal } from "react-dom";
import { useLocation, useNavigate } from "react-router";
import type { ModelChoice } from "../data/model-selection";
import { restorePreviousFocus, useAnimatedDialogClose, useStandbyDialogClose } from "../shell/app-dialogs";
import { useDialogFocusTrap } from "../shell/dialog-focus";
import { CSTOA_PROVIDER, loginPath, returnPath } from "./providers";

export type ModelKeyDialogReason = "initial" | "unsent" | "missing";

export function ModelKeyDialog({
	model,
	reason = "missing",
	onClose,
}: {
	model: ModelChoice;
	reason?: ModelKeyDialogReason;
	onClose: () => void;
}) {
	const dialog = useRef<HTMLDialogElement>(null);
	const cancelRef = useRef<HTMLButtonElement>(null);
	const titleId = useId();
	const descriptionId = useId();
	const navigate = useNavigate();
	const location = useLocation();
	const providerName = model.providerName ?? model.provider;
	const destination = returnPath(`${location.pathname}${location.search}`);
	const { requestClose, closing } = useAnimatedDialogClose(dialog);
	useDialogFocusTrap(dialog, cancelRef);
	useStandbyDialogClose(dialog, onClose);
	useEffect(() => {
		const previous = document.activeElement;
		const node = dialog.current;
		node?.showModal();
		return () => restorePreviousFocus(node, previous);
	}, []);
	function leave() {
		requestClose(onClose);
	}
	function openProviderPage(method?: "api_key") {
		const params = new URLSearchParams({
			provider: model.provider,
			from: "model-key",
			returnTo: destination,
		});
		if (method) params.set("method", method);
		requestClose(() => {
			onClose();
			void navigate(`/settings/provider?${params}`);
		});
	}
	function login() {
		requestClose(() => {
			onClose();
			void navigate(loginPath(CSTOA_PROVIDER, "oauth", destination));
		});
	}
	return createPortal(
		<dialog
			className="tier-3-dialog exit-dialog model-key-dialog"
			ref={dialog}
			aria-labelledby={titleId}
			aria-describedby={descriptionId}
			onCancel={(event) => {
				event.preventDefault();
				leave();
			}}
		>
			<header className="dialog-title-row dialog-title-row--warning model-key-heading">
				<KeyRound size={16} aria-hidden="true" />
				<h2 id={titleId}>{reason === "initial" ? "尚未配置模型" : `配置 ${providerName} 密钥`}</h2>
				<button
					type="button"
					className="btn-icon sm-28 model-key-close"
					aria-label="关闭密钥提示"
					disabled={closing}
					onClick={leave}
				>
					<X size={16} aria-hidden="true" />
				</button>
			</header>
			{reason !== "initial" && (
				<div className="model-key-identity">
					<strong>{model.name}</strong>
					<span>
						{providerName} · {model.id}
					</span>
				</div>
			)}
			<p id={descriptionId} className="exit-description">
				{reason === "initial"
					? "登录 CSTOA 可直接使用内置模型；其他服务请在模型服务页填写 APIKEY。"
					: reason === "unsent"
						? "尚未配置 API key。消息未发送，草稿已保留。"
						: "尚未配置 API key，配置后即可开始对话。"}
			</p>
			<div className="exit-actions">
				<button ref={cancelRef} type="button" className="tier-1-btn-outline" disabled={closing} onClick={leave}>
					暂不配置
				</button>
				{reason === "initial" ? (
					<>
						<button
							type="button"
							className="tier-1-btn-outline"
							disabled={closing}
							onClick={() => openProviderPage("api_key")}
						>
							填写 APIKEY
						</button>
						<button
							type="button"
							className="tier-1-btn-primary model-key-configure"
							disabled={closing}
							onClick={login}
						>
							登录 CSTOA
						</button>
					</>
				) : (
					<button
						type="button"
						className="tier-1-btn-primary model-key-configure"
						disabled={closing}
						onClick={() => openProviderPage()}
					>
						配置密钥
					</button>
				)}
			</div>
		</dialog>,
		document.body,
	);
}
