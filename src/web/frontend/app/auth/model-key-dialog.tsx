import { KeyRound, X } from "lucide-react";
import { useEffect, useId, useMemo, useRef } from "react";
import { createPortal } from "react-dom";
import { useLocation, useNavigate } from "react-router";
import type { ModelChoice } from "../data/model-selection";
import { restorePreviousFocus, useAnimatedDialogClose, useStandbyDialogClose } from "../shell/app-dialogs";
import { useDialogFocusTrap } from "../shell/dialog-focus";
import { type DialogActionItem, KbdFocusOverlay, useDialogActionNav } from "../shell/kbd-focus";
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
	const actions = useMemo<readonly DialogActionItem[]>(
		() =>
			reason === "initial"
				? [
						{ id: "cancel", tone: "neutral" },
						{ id: "apikey", tone: "neutral" },
						{ id: "primary", tone: "accent" },
					]
				: [
						{ id: "cancel", tone: "neutral" },
						{ id: "primary", tone: "accent" },
					],
		[reason],
	);
	const actionNav = useDialogActionNav(actions, { initialId: "cancel", disabled: closing });
	useDialogFocusTrap(dialog, cancelRef);
	useStandbyDialogClose(dialog, onClose);
	useEffect(() => {
		const previous = document.activeElement;
		const node = dialog.current;
		node?.showModal();
		cancelRef.current?.focus();
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
			onKeyDown={actionNav.onDialogKeyDown}
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
			<div
				ref={actionNav.containerRef}
				className="exit-actions kbd-focus-scope kbd-dialog-actions"
				data-kbd-active={actionNav.kbdActive ? "true" : "false"}
			>
				<button
					ref={actionNav.registerButton("cancel", cancelRef)}
					type="button"
					className="tier-1-btn-outline"
					disabled={closing}
					onPointerDown={actionNav.onButtonPointerDown}
					onFocus={() => actionNav.onButtonFocus("cancel")}
					onClick={leave}
				>
					暂不配置
				</button>
				{reason === "initial" ? (
					<>
						<button
							ref={actionNav.registerButton("apikey")}
							type="button"
							className="tier-1-btn-outline"
							disabled={closing}
							onPointerDown={actionNav.onButtonPointerDown}
							onFocus={() => actionNav.onButtonFocus("apikey")}
							onClick={() => openProviderPage("api_key")}
						>
							填写 APIKEY
						</button>
						<button
							ref={actionNav.registerButton("primary")}
							type="button"
							className="tier-1-btn-primary model-key-configure"
							disabled={closing}
							onPointerDown={actionNav.onButtonPointerDown}
							onFocus={() => actionNav.onButtonFocus("primary")}
							onClick={login}
						>
							登录 CSTOA
						</button>
					</>
				) : (
					<button
						ref={actionNav.registerButton("primary")}
						type="button"
						className="tier-1-btn-primary model-key-configure"
						disabled={closing}
						onPointerDown={actionNav.onButtonPointerDown}
						onFocus={() => actionNav.onButtonFocus("primary")}
						onClick={() => openProviderPage()}
					>
						配置密钥
					</button>
				)}
				<KbdFocusOverlay
					containerRef={actionNav.containerRef}
					itemRefs={actionNav.itemRefs}
					targetId={actionNav.targetId}
					style={actionNav.kbdStyle}
					tone={actionNav.activeTone}
					railPlacement="bottom"
					bracketOutset={3}
				/>
			</div>
		</dialog>,
		document.body,
	);
}
