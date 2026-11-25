import { ArrowLeft } from "lucide-react";
import { useCallback, useRef } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { ProviderLoginForm } from "../auth/provider-login-form";
import { returnPath } from "../auth/providers";
import { BlueHour } from "../shell/blue-hour";

export default function Login() {
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const destination = returnPath(params.get("returnTo"));
	const backHandler = useRef<(() => boolean) | null>(null);
	const handleBack = useCallback(() => {
		if (backHandler.current?.()) return;
		void navigate(destination);
	}, [navigate, destination]);
	return (
		<div className="login-scene">
			<div className="login-sky" aria-hidden="true">
				<BlueHour kind="sky" />
				<div className="login-sky-feather" />
			</div>
			<button
				type="button"
				className="btn-ghost login-back"
				aria-label="返回"
				aria-keyshortcuts="Escape"
				onClick={handleBack}
			>
				<ArrowLeft size={16} aria-hidden="true" />
				返回
			</button>
			<main className="login-panel" aria-label="登录">
				<div className="login-top-spacer" aria-hidden="true" />
				<div className="login-viewport">
					<ProviderLoginForm backHandlerRef={backHandler} />
				</div>
				<footer className="login-footnote">
					<span>@cst-pilot</span>
				</footer>
			</main>
		</div>
	);
}
