import { Button } from "@heroui/react";
import { ArrowLeft, Pencil } from "lucide-react";
import { useNavigate, useSearchParams } from "react-router";
import { ProviderLoginForm } from "../auth/provider-login-form";
import { returnPath } from "../auth/providers";

export default function ProviderConfiguration() {
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const destination = returnPath(params.get("returnTo") ?? "/settings#accounts");
	return (
		<main className="chat-main settings-main provider-config-main">
			<header className="chat-header settings-header">
				<Pencil size={22} aria-hidden="true" />
				<h1>配置模型服务</h1>
				<Button
					type="button"
					variant="ghost"
					isIconOnly
					className="provider-config-close"
					aria-label="返回设置"
					aria-keyshortcuts="Escape"
					onPress={() => void navigate(destination)}
				>
					<ArrowLeft size={22} aria-hidden="true" />
				</Button>
			</header>
			<div className="settings-scroll">
				<section className="settings-content provider-config-content" aria-labelledby="provider-group-title">
					<header className="settings-group-head">
						<h2 id="provider-group-title">模型服务</h2>
					</header>
					<ProviderLoginForm standalone />
				</section>
			</div>
		</main>
	);
}
