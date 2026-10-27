import { ChevronRight, Info } from "lucide-react";
import { type CSSProperties, useId, useLayoutEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { formatQuotaBalance, quotaProviders } from "./quota-format";
import { useProviderQuota } from "./use-quota";

type ContextUsage = { tokens: number | null; contextWindow: number; percent: number | null } | null;
const windowNames = { rolling: "5 小时", weekly: "本周", monthly: "本月" } as const;

export function ContextMeter({
	usage,
	cacheHitRate,
	provider,
}: {
	usage: ContextUsage;
	cacheHitRate: number | null;
	provider: string;
}) {
	const percent = usage?.percent ?? 0;
	const panelId = useId();
	const panel = useRef<HTMLElement>(null);
	const trigger = useRef<HTMLButtonElement>(null);
	const [hovered, setHovered] = useState(false);
	const [focused, setFocused] = useState(false);
	const [dismissed, setDismissed] = useState(false);
	const open = (hovered || focused) && !dismissed;
	usePanelDismiss(open, () => {
		if (panel.current?.contains(document.activeElement)) trigger.current?.focus();
		setDismissed(true);
	});
	const { quota: current, loading, retry } = useProviderQuota(provider, open);
	const showQuota = quotaProviders.has(provider);
	const balance = formatQuotaBalance(current);
	// biome-ignore lint/correctness/useExhaustiveDependencies: 模型服务变化会移动触发按钮，打开面板时重新计算视口边界。
	useLayoutEffect(() => {
		if (!open) return;
		const place = () => {
			const element = panel.current;
			if (!element) return;
			element.style.removeProperty("--context-shift");
			const rect = element.getBoundingClientRect();
			const shift =
				rect.left < 16
					? 16 - rect.left
					: rect.right > window.innerWidth - 16
						? window.innerWidth - 16 - rect.right
						: 0;
			element.style.setProperty("--context-shift", `${shift}px`);
		};
		place();
		window.addEventListener("resize", place);
		return () => window.removeEventListener("resize", place);
	}, [open, provider]);

	return (
		<fieldset
			className="context-meter-wrap"
			aria-label="上下文"
			data-open={open}
			onMouseEnter={() => {
				setHovered(true);
				setDismissed(false);
			}}
			onMouseLeave={() => setHovered(false)}
			onFocusCapture={() => {
				setFocused(true);
				setDismissed(false);
			}}
			onBlurCapture={(event) => {
				if (!(event.relatedTarget instanceof Node) || !event.currentTarget.contains(event.relatedTarget))
					setFocused(false);
			}}
		>
			<button
				ref={trigger}
				type="button"
				className="context-meter"
				aria-expanded={open}
				aria-controls={panelId}
				aria-label={usage?.percent == null ? "上下文用量未知" : `上下文用量 ${Math.round(percent)}%`}
			>
				<span
					className="context-ring"
					style={{ "--context-progress": `${Math.max(0, Math.min(100, percent))}%` } as CSSProperties}
					data-level={percent >= 90 ? "danger" : percent >= 75 ? "warning" : "normal"}
				/>
			</button>
			<section className="context-panel" ref={panel} id={panelId} aria-label="上下文信息">
				<span className="context-row">
					<span className="context-label">上下文容量</span>
					<span className="context-value">
						{usage?.tokens == null
							? "用量未知"
							: `${usage.tokens.toLocaleString("zh-CN")} / ${usage.contextWindow.toLocaleString("zh-CN")}（${usage.percent == null ? "占比未知" : `${usage.percent.toFixed(1)}%`}）`}
					</span>
				</span>
				<span className="context-progress" aria-hidden="true">
					<span style={{ width: `${Math.max(0, Math.min(100, percent))}%` }} />
				</span>
				<span className="context-row context-row--sub">
					<span className="context-label">平均缓存命中率</span>
					<span className="context-value">
						{cacheHitRate == null ? "暂无缓存数据" : `${(cacheHitRate * 100).toFixed(1)}%`}
					</span>
				</span>
				{showQuota && (
					<>
						<span className="context-divider" aria-hidden="true" />
						<span className="context-title-row">
							<span className="context-title">剩余额度</span>
							<span
								className="context-info"
								title={
									provider === "opencode-go"
										? "额度按 5 小时、每周、每月三个窗口计算"
										: provider === "cstoa"
											? "CSTOA 账号额度；10,000 额度点 = ¥1"
											: "DeepSeek 账户可用余额"
								}
							>
								<Info size={14} aria-hidden="true" />
							</span>
							{provider === "cstoa" ? (
								<Link className="context-more" to="/account" viewTransition>
									账号信息
									<ChevronRight size={14} aria-hidden="true" />
								</Link>
							) : (
								<a
									className="context-more"
									href={
										provider === "deepseek" ? "https://platform.deepseek.com/" : "https://opencode.ai/auth"
									}
									target="_blank"
									rel="noreferrer"
								>
									更多
									<ChevronRight size={14} aria-hidden="true" />
								</a>
							)}
						</span>
						{balance !== null ? (
							<span className="context-balance" data-unit={current?.unit}>
								{balance}
							</span>
						) : null}
						{provider === "opencode-go" && current?.supported && current.windows
							? (Object.keys(windowNames) as (keyof typeof windowNames)[])
									.filter((key) => current.windows?.[key])
									.map((key) => {
										const window = current.windows?.[key];
										return (
											<span className="context-row context-row--sub" key={key}>
												<span className="context-label">{windowNames[key]}</span>
												<span
													className="context-value"
													title={
														window?.resetsAt
															? `重置时间 ${new Date(window.resetsAt).toLocaleString("zh-CN")}`
															: undefined
													}
												>{`剩余 ${window?.status === "rate-limited" ? 0 : Math.max(0, Math.round(100 - (window?.percent ?? 0)))}%`}</span>
											</span>
										);
									})
							: null}
						{!current?.supported && (
							<div className="context-quota-notice" aria-live="polite">
								<span>{loading ? "正在查询额度…" : (current?.reason ?? "暂无额度数据")}</span>
								{current && !current.requiresLogin ? (
									<button type="button" className="context-more" disabled={loading} onClick={retry}>
										重试
									</button>
								) : null}
							</div>
						)}
					</>
				)}
			</section>
		</fieldset>
	);
}
