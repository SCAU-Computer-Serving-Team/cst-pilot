import { ChevronRight, CircleAlert, KeyRound, WifiOff } from "lucide-react";
import { Link } from "react-router";
import { presentError } from "../data/error-presenter";

export function ErrorNotice({
	message,
	onRetry,
	model = false,
	input = false,
}: {
	message: string;
	onRetry?: () => void;
	model?: boolean;
	input?: boolean;
}) {
	const error = presentError(message, { model, input });
	const Icon = error.kind === "auth" ? KeyRound : error.kind === "connection" ? WifiOff : CircleAlert;
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
				<pre>{error.detail}</pre>
			</details>
		</div>
	);
}
