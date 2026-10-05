import { Button } from "@heroui/react";
import { X } from "lucide-react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { ProviderLoginForm } from "../auth/provider-login-form";
import { returnPath } from "../auth/providers";
import { BlueHour } from "../shell/blue-hour";
import { HomeSurface } from "../shell/shell";

export default function Login() {
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const destination = returnPath(params.get("returnTo"));
	return (
		<div className="login-scene">
			<div className="login-behind" aria-hidden="true" inert>
				<HomeSurface paused />
			</div>
			<div className="login-shade" aria-hidden="true" />
			<main className="login-panel" aria-label="登录">
				<BlueHour kind="air" />
				<div className="login-bottom-gradient" aria-hidden="true" />
				<Button
					type="button"
					variant="ghost"
					isIconOnly
					className="login-close"
					aria-label="关闭登录"
					aria-keyshortcuts="Escape"
					onPress={() => void navigate(destination)}
				>
					<X size={22} aria-hidden="true" />
				</Button>
				<Link className="login-brand" to={destination}>
					CST Pilot
				</Link>
				<div className="login-heading">
					<h1>欢迎回来</h1>
				</div>
				<ProviderLoginForm />
				<span className="login-footnote">@cst-pilot-web</span>
			</main>
		</div>
	);
}
