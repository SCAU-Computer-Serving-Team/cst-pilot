import { Button, Checkbox, Input, Label, ListBox, Select, TextField } from "@heroui/react";
import { Check, ChevronDown, Eye, EyeOff } from "lucide-react";
import { type FormEvent, useEffect, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { apiJson, type ProviderStatus } from "../app/api";
import { BlueHour } from "../app/blue-hour";
import { HomeSurface } from "../app/shell";

export default function Login() {
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const [providers, setProviders] = useState<ProviderStatus[]>([]);
	const [provider, setProvider] = useState("");
	const [customProvider, setCustomProvider] = useState(false);
	const [baseUrl, setBaseUrl] = useState("");
	const [apiKey, setApiKey] = useState("");
	const [showKey, setShowKey] = useState(false);
	const [loading, setLoading] = useState(true);
	const [submitting, setSubmitting] = useState(false);
	const [error, setError] = useState("");

	// 页面初开时可带 ?provider= 指定默认选中的模型服务。
	const requestedProvider = params.get("provider");
	useEffect(() => {
		const controller = new AbortController();
		apiJson<{ providers: ProviderStatus[] }>("/api/auth", { signal: controller.signal })
			.then((data) => {
				const apiKeyProviders = data.providers.filter((item) => item.supportsApiKey);
				setProviders(apiKeyProviders);
				setProvider(
					(current) =>
						current ||
						apiKeyProviders.find((item) => item.id === requestedProvider)?.id ||
						apiKeyProviders.find((item) => item.id === "cst")?.id ||
						apiKeyProviders[0]?.id ||
						"",
				);
			})
			.catch((cause: unknown) => {
				if (!controller.signal.aborted)
					setError(cause instanceof Error ? cause.message : "无法读取模型服务，请重试。");
			})
			.finally(() => {
				if (!controller.signal.aborted) setLoading(false);
			});
		return () => controller.abort();
	}, [requestedProvider]);

	async function submit(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		if (submitting || loading || !provider || !apiKey.trim() || (customProvider && !baseUrl.trim())) return;
		setSubmitting(true);
		setError("");
		try {
			await apiJson(`/api/auth/${encodeURIComponent(provider)}/api-key`, {
				method: "PUT",
				body: { key: apiKey, ...(customProvider ? { baseUrl: baseUrl.trim() } : {}) },
			});
			setApiKey("");
			navigate("/");
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "登录未完成，请重试。");
		} finally {
			setSubmitting(false);
		}
	}

	return (
		<div className="login-scene">
			<div className="login-behind" aria-hidden="true" inert>
				<HomeSurface />
			</div>
			<div className="login-shade" aria-hidden="true" />
			<main className="login-panel" aria-label="登录">
				<BlueHour kind="air" />
				<div className="login-bottom-gradient" aria-hidden="true" />
				<Link className="login-brand" to="/">
					CST Pilot
				</Link>
				<div className="login-heading">
					<h1>欢迎回来</h1>
					<p>登录以同步模型配置与额度</p>
				</div>
				<form className="login-card" onSubmit={submit}>
					<section className="login-switch" aria-label="登录方式">
						<span className="login-switch-inactive">扫码登录</span>
						<span className="login-switch-active">APIKEY</span>
					</section>
					<div className="login-fields">
						<Select
							selectedKey={provider || null}
							onSelectionChange={(key) => {
								if (key != null) {
									setProvider(String(key));
									setError("");
								}
							}}
							isDisabled={loading || !providers.length}
							className="login-field login-provider"
						>
							<Label>Provider</Label>
							<Select.Trigger className="login-input">
								<Select.Value>
									{providers.find((item) => item.id === provider)?.name ?? "选择 Provider"}
								</Select.Value>
								<ChevronDown size={16} aria-hidden="true" />
							</Select.Trigger>
							<Select.Popover className="login-provider-popover" placement="bottom start">
								<ListBox className="login-provider-list">
									{providers.map((item) => (
										<ListBox.Item
											key={item.id}
											id={item.id}
											textValue={item.name}
											className="login-provider-option"
										>
											{item.name}
											<span className="login-provider-check">
												<Check size={16} aria-hidden="true" />
											</span>
										</ListBox.Item>
									))}
								</ListBox>
							</Select.Popover>
						</Select>
						<Checkbox
							isSelected={customProvider}
							onChange={(selected) => {
								setCustomProvider(selected);
								setError("");
							}}
							className="login-custom-checkbox"
						>
							<Checkbox.Content>
								<Checkbox.Control>
									<Checkbox.Indicator>
										<Check size={13} aria-hidden="true" />
									</Checkbox.Indicator>
								</Checkbox.Control>
								<Label>自定义 Provider</Label>
							</Checkbox.Content>
						</Checkbox>
						{customProvider && (
							<TextField className="login-field" value={baseUrl} onChange={setBaseUrl}>
								<Label>BaseURL</Label>
								<Input
									className="login-input"
									type="url"
									placeholder="https://api.example.com/v1"
									autoComplete="url"
								/>
							</TextField>
						)}
						<TextField className="login-field" value={apiKey} onChange={setApiKey}>
							<Label>APIKEY</Label>
							<div className="login-key-field">
								<Input
									className="login-input"
									type={showKey ? "text" : "password"}
									placeholder="输入 APIKEY"
									autoComplete="off"
									spellCheck={false}
								/>
								<Button
									variant="ghost"
									isIconOnly
									className="login-key-visibility"
									onPress={() => setShowKey((visible) => !visible)}
									aria-label={showKey ? "隐藏 APIKEY" : "显示 APIKEY"}
								>
									{showKey ? <EyeOff size={16} /> : <Eye size={16} />}
								</Button>
							</div>
						</TextField>
					</div>
					{error && (
						<p className="login-message login-message--error" role="alert">
							{error}
						</p>
					)}
					<Button
						type="submit"
						isDisabled={
							loading || submitting || !provider || !apiKey.trim() || (customProvider && !baseUrl.trim())
						}
						className="login-submit"
					>
						{submitting ? "正在登录…" : "登录"}
					</Button>
				</form>
				<span className="login-footnote">@cst-pilot-web</span>
			</main>
		</div>
	);
}
