import { ChevronRight, CircleAlert, Info, KeyRound, WifiOff, X } from "lucide-react";
import { Link } from "react-router";
import { presentError } from "../data/error-presenter";
import { CodeBlock } from "./code-block";
import { Notification, NotificationDetail } from "./notification";

export function ErrorNotice({
	message,
	onRetry,
	model = false,
	input = false,
	onDismiss,
}: {
	message: string;
	onRetry?: () => void;
	model?: boolean;
	input?: boolean;
	onDismiss?: () => void;
}) {
	const error = presentError(message, { model, input });
	const Icon =
		error.kind === "auth"
			? KeyRound
			: error.kind === "connection"
				? WifiOff
				: error.kind === "compaction"
					? Info
					: CircleAlert;
	if (onDismiss)
		return (
			<Notification
				className="error-notification"
				label="操作提示"
				kind={
					error.kind === "compaction"
						? "info"
						: error.kind === "auth" || error.kind === "busy" || error.kind === "connection"
							? "warning"
							: "danger"
				}
				duration={2600}
				onDismiss={onDismiss}
			>
				<NotificationDetail icon={<Icon size={18} aria-hidden="true" />}>
					{error.description && <span className="notification-detail-description">{error.description}</span>}
					<span className="notification-detail-label">技术详情</span>
					<CodeBlock>{error.detail}</CodeBlock>
				</NotificationDetail>
				<span className="notification-message">{error.title}</span>
				{error.kind === "auth" ? (
					<Link className="notice-action" to="/settings#accounts">
						重新登录
					</Link>
				) : model ? (
					<Link className="notice-action" to="/settings">
						模型设置
					</Link>
				) : null}
				<button type="button" className="notification-close" aria-label="关闭操作提示" onClick={onDismiss}>
					<X size={18} aria-hidden="true" />
				</button>
			</Notification>
		);
	return (
		<div className="error-state">
			<section className="error-notice" data-kind={error.kind} role="alert">
				<Icon size={18} aria-hidden="true" />
				<div className="error-notice-body">
					<strong>{error.title}</strong>
					{error.description && <p>{error.description}</p>}
				</div>
				{error.kind === "auth" ? (
					<Link className="notice-action" to="/settings#accounts">
						重新登录
					</Link>
				) : onRetry ? (
					<button type="button" className="notice-action" onClick={onRetry}>
						{error.kind === "connection" ? "重新连接" : "重试"}
					</button>
				) : model ? (
					<Link className="notice-action" to="/settings">
						模型设置
					</Link>
				) : null}
			</section>
			<details className="error-details">
				<summary>
					技术详情 <ChevronRight size={14} aria-hidden="true" />
				</summary>
				<CodeBlock>{error.detail}</CodeBlock>
			</details>
		</div>
	);
}
