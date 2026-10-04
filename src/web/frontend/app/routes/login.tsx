import { Button, Checkbox, Input, Label, ListBox, Select, TextField } from "@heroui/react";
import { Check, ChevronDown, Eye, EyeOff } from "lucide-react";
import { QRCodeSVG } from "qrcode.react";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import {
	apiJson,
	cancelOauthLogin,
	getOauthStatus,
	type OauthDeviceCode,
	type ProviderStatus,
	startOauthLogin,
} from "../app/api";
import { BlueHour } from "../app/blue-hour";
import { HomeSurface } from "../app/shell";

function formatCountdown(seconds: number): string {
	const minutes = Math.floor(seconds / 60);
	return `${minutes}:${String(seconds % 60).padStart(2, "0")}`;
}

export default function Login() {
	const navigate = useNavigate();
	const [params] = useSearchParams();
	const [providers, setProviders] = useState<ProviderStatus[]>([]);
	const [oauthProviders, setOauthProviders] = useState<ProviderStatus[]>([]);
	const [mode, setMode] = useState<"qr" | "apikey">("apikey");
	const [provider, setProvider] = useState("");
	const [customProvider, setCustomProvider] = useState(false);
	const [baseUrl, setBaseUrl] = useState("");
	const [apiKey, setApiKey] = useState("");
	const [showKey, setShowKey] = useState(false);
	const [loading, setLoading] = useState(true);
	const [submitting, setSubmitting] = useState(false);
	const [error, setError] = useState("");
	const [oauthProvider, setOauthProvider] = useState("");
	const [qrState, setQrState] = useState<"starting" | "pending" | "failed" | "cancelled">("starting");
	const [deviceCode, setDeviceCode] = useState<OauthDeviceCode | null>(null);
	const [qrError, setQrError] = useState("");
	const [expiresAt, setExpiresAt] = useState(0);
	const [qrBusy, setQrBusy] = useState(false);
	const [clock, setClock] = useState(() => Date.now());
	const acceptedDeviceCodeRef = useRef("");

	useEffect(() => {
		const timer = window.setInterval(() => setClock(Date.now()), 1000);
		return () => window.clearInterval(timer);
	}, []);

	// 页面初开时可带 ?provider= 指定默认选中的模型服务。
	const requestedProvider = params.get("provider");
	useEffect(() => {
		const controller = new AbortController();
		apiJson<{ providers: ProviderStatus[] }>("/api/auth", { signal: controller.signal })
			.then((data) => {
				const apiKeyProviders = data.providers.filter((item) => item.supportsApiKey);
				// 扫码面板按设备码流实现，目前只有团队 provider cstoa 走该流程。
				const qrProviders = data.providers.filter((item) => item.supportsOAuth && item.id === "cstoa");
				setProviders(apiKeyProviders);
				setOauthProviders(qrProviders);
				setOauthProvider((current) => current || qrProviders[0]?.id || "");
				// 有可扫码的模型服务时默认展示扫码面板，进入即自动申请设备码。
				if (qrProviders.length) setMode("qr");
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

	// 扫码登录：进入扫码模式即自动申请设备码；进行中的流程会被复用（start 幂等）。
	useEffect(() => {
		if (mode !== "qr" || !oauthProvider) return;
		let disposed = false;
		setQrState("starting");
		setQrError("");
		const events = new EventSource("/api/events");
		const acceptDeviceCode = (value: OauthDeviceCode) => {
			if (disposed) return;
			if (acceptedDeviceCodeRef.current === value.userCode) return;
			acceptedDeviceCodeRef.current = value.userCode;
			const seconds = value.expiresInSeconds || 300;
			setDeviceCode(value);
			setExpiresAt(Date.now() + seconds * 1000);
			setQrState("pending");
			setQrError("");
		};
		events.addEventListener("state", (raw) => {
			if (disposed) return;
			let event: {
				type?: string;
				providerId?: string;
				ok?: boolean;
				cancelled?: boolean;
				error?: string;
			} & Partial<OauthDeviceCode> = {};
			try {
				event = JSON.parse((raw as MessageEvent).data as string) as typeof event;
			} catch {
				return;
			}
			if (event.providerId !== oauthProvider) return;
			if (event.type === "oauth_device_code") {
				acceptDeviceCode({
					userCode: event.userCode ?? "",
					verificationUri: event.verificationUri ?? "",
					intervalSeconds: event.intervalSeconds ?? 5,
					expiresInSeconds: event.expiresInSeconds ?? 300,
				});
			} else if (event.type === "oauth_result") {
				if (event.ok) navigate("/");
				else if (event.cancelled) setQrState("cancelled");
				else {
					setQrState("failed");
					setQrError(event.error ?? "登录未完成，请重试。");
				}
			}
		});
		void startOauthLogin(oauthProvider)
			.then((result) => {
				if (disposed) return;
				if (result.deviceCode) acceptDeviceCode(result.deviceCode);
			})
			.catch((cause: unknown) => {
				if (disposed) return;
				setQrState("failed");
				setQrError(cause instanceof Error ? cause.message : "无法发起扫码登录，请重试。");
			});
		const poll = window.setInterval(() => {
			void getOauthStatus(oauthProvider)
				.then((status) => {
					if (disposed) return;
					if (status.state === "pending" && status.deviceCode) acceptDeviceCode(status.deviceCode);
					else if (status.state === "succeeded") navigate("/");
					else if (status.state === "failed") {
						setQrState("failed");
						setQrError(status.error ?? "登录未完成，请重试。");
					} else if (status.state === "cancelled") setQrState("cancelled");
				})
				.catch(() => undefined);
		}, 2000);
		return () => {
			disposed = true;
			acceptedDeviceCodeRef.current = "";
			window.clearInterval(poll);
			events.close();
		};
	}, [mode, oauthProvider, navigate]);

	const restartQr = async () => {
		if (!oauthProvider || qrBusy) return;
		setQrBusy(true);
		setQrError("");
		try {
			await cancelOauthLogin(oauthProvider).catch(() => undefined);
			setDeviceCode(null);
			setExpiresAt(0);
			acceptedDeviceCodeRef.current = "";
			setQrState("starting");
			const result = await startOauthLogin(oauthProvider);
			if (result.deviceCode) {
				setDeviceCode(result.deviceCode);
				setExpiresAt(Date.now() + result.deviceCode.expiresInSeconds * 1000);
				setQrState("pending");
			}
		} catch (cause) {
			setQrState("failed");
			setQrError(cause instanceof Error ? cause.message : "无法获取二维码，请重试。");
		} finally {
			setQrBusy(false);
		}
	};

	const cancelQr = async () => {
		if (!oauthProvider) return;
		acceptedDeviceCodeRef.current = "";
		setQrState("cancelled");
		setDeviceCode(null);
		await cancelOauthLogin(oauthProvider).catch(() => undefined);
	};

	const remaining = expiresAt ? Math.max(0, Math.ceil((expiresAt - clock) / 1000)) : 0;
	const qrExpired = qrState === "pending" && expiresAt > 0 && remaining <= 0;
	const qrWaiting = qrState === "starting" || (qrState === "pending" && !deviceCode);
	const qrErrorText = qrState === "failed" ? qrError || "扫码登录未完成，请重试。" : "";

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
						<button
							type="button"
							className={mode === "qr" ? "login-switch-active" : "login-switch-inactive"}
							disabled={!oauthProviders.length}
							onClick={() => {
								setMode("qr");
								setError("");
							}}
						>
							扫码登录
						</button>
						<button
							type="button"
							className={mode === "apikey" ? "login-switch-active" : "login-switch-inactive"}
							onClick={() => {
								setMode("apikey");
								setError("");
							}}
						>
							APIKEY
						</button>
					</section>
					{mode === "qr" ? (
						<div className="login-fields login-fields--qr">
							{qrWaiting && <p className="login-message login-message--inline">正在获取二维码…</p>}
							{qrState === "pending" && deviceCode && !qrExpired && (
								<>
									<div className="login-qr">
										<div className="login-qr-box">
											<QRCodeSVG
												value={deviceCode.verificationUri}
												size={168}
												level="M"
												bgColor="#ffffff"
												fgColor="#1a2024"
												title="OA 授权二维码"
											/>
										</div>
										<p className="login-qr-hint">用手机扫码，在 OA 中确认授权</p>
										<p className="login-qr-code">或手动输入：{deviceCode.userCode}</p>
										<p className="login-qr-countdown">二维码剩余 {formatCountdown(remaining)}</p>
									</div>
									<div className="login-qr-actions">
										<Button
											type="button"
											variant="ghost"
											isDisabled={qrBusy}
											onPress={() => void restartQr()}
										>
											刷新二维码
										</Button>
										<Button type="button" variant="ghost" onPress={() => void cancelQr()}>
											取消
										</Button>
									</div>
								</>
							)}
							{qrExpired && (
								<>
									<p className="login-message login-message--inline">二维码已过期，请刷新后重试。</p>
									<Button
										type="button"
										className="login-submit"
										isDisabled={qrBusy}
										onPress={() => void restartQr()}
									>
										{qrBusy ? "正在获取…" : "刷新二维码"}
									</Button>
								</>
							)}
							{qrState === "cancelled" && (
								<>
									<p className="login-message login-message--inline">已取消本次扫码登录。</p>
									<Button
										type="button"
										className="login-submit"
										isDisabled={qrBusy}
										onPress={() => void restartQr()}
									>
										重新获取二维码
									</Button>
								</>
							)}
							{qrErrorText && (
								<>
									<p className="login-message login-message--error login-message--inline" role="alert">
										{qrErrorText}
									</p>
									<Button
										type="button"
										className="login-submit"
										isDisabled={qrBusy}
										onPress={() => void restartQr()}
									>
										重试
									</Button>
								</>
							)}
						</div>
					) : (
						<>
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
						</>
					)}
				</form>
				<span className="login-footnote">@cst-pilot-web</span>
			</main>
		</div>
	);
}
