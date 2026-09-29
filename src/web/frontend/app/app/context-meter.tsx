import { ChevronRight, Info } from "lucide-react";
import { type CSSProperties, useEffect, useRef, useState } from "react";
import { apiJson } from "./api";

type Quota = {
  provider: string;
  supported: boolean;
  reason?: string;
  windows?: Record<string, { status: string; percent: number; resetsAt: string | null }>;
  balance?: number | null;
  currency?: string | null;
};

type ContextUsage = { tokens: number | null; contextWindow: number; percent: number | null } | null;
const windowNames = { rolling: "5 小时", weekly: "本周", monthly: "本月" } as const;
const supportedProviders = new Set(["deepseek", "opencode-go"]);

function amount(balance: number, currency: string | null | undefined): string {
  return `${currency === "USD" ? "US$" : "¥"} ${balance.toFixed(2)}`;
}

export function ContextMeter({ usage, cacheHitRate, provider }: {
  usage: ContextUsage;
  cacheHitRate: number | null;
  provider: string;
}) {
  const percent = usage?.percent ?? 0;
  const [quota, setQuota] = useState<Quota | null>(null);
  const [refresh, setRefresh] = useState(0);
  const requestedAt = useRef(0);
  useEffect(() => {
    setQuota(null);
    if (!supportedProviders.has(provider)) return;
    let active = true;
    requestedAt.current = Date.now();
    apiJson<Quota>(`/api/quota?provider=${encodeURIComponent(provider)}`)
      .then((data) => { if (active) setQuota(data); })
      .catch(() => { if (active) setQuota({ provider, supported: false, reason: "额度查询失败" }); });
    return () => { active = false; };
  }, [provider, refresh]);
  const current = quota?.provider === provider ? quota : null;
  const showQuota = supportedProviders.has(provider);
  const refreshQuota = () => {
    if (showQuota && Date.now() - requestedAt.current >= 60_000) setRefresh((value) => value + 1);
  };

  return <span className="context-meter-wrap" onMouseEnter={refreshQuota} onFocus={refreshQuota}>
    <button type="button" className="context-meter" aria-label={usage?.percent == null ? "上下文用量未知" : `上下文用量 ${Math.round(percent)}%`}>
      <span className="context-ring" style={{ "--context-progress": `${Math.max(0, Math.min(100, percent))}%` } as CSSProperties} data-level={percent >= 90 ? "danger" : percent >= 75 ? "warning" : "normal"} />
    </button>
    <span className="context-panel" role="tooltip">
      <span className="context-row"><span className="context-label">上下文容量</span><span className="context-value">{usage?.tokens == null ? "用量未知" : `${usage.tokens.toLocaleString("zh-CN")} / ${usage.contextWindow.toLocaleString("zh-CN")}（${usage.percent == null ? "占比未知" : `${usage.percent.toFixed(1)}%`}）`}</span></span>
      <span className="context-progress" aria-hidden="true"><span style={{ width: `${Math.max(0, Math.min(100, percent))}%` }} /></span>
      <span className="context-row context-row--sub"><span className="context-label">平均缓存命中率</span><span className="context-value">{cacheHitRate == null ? "暂无缓存数据" : `${(cacheHitRate * 100).toFixed(1)}%`}</span></span>
      {showQuota && <>
        <span className="context-divider" aria-hidden="true" />
        <span className="context-title-row">
          <span className="context-title">剩余额度</span>
          <span className="context-info" title={provider === "opencode-go" ? "额度按 5 小时、每周、每月三个窗口计算" : "DeepSeek 账户可用余额"}><Info size={13} aria-hidden="true" /></span>
          <a className="context-more" href={provider === "deepseek" ? "https://platform.deepseek.com/" : "https://opencode.ai/auth"} target="_blank" rel="noreferrer">更多<ChevronRight size={14} aria-hidden="true" /></a>
        </span>
        {provider === "deepseek" && typeof current?.balance === "number" && current.supported
          ? <span className="context-balance">{amount(current.balance, current.currency)}</span>
          : null}
        {provider === "opencode-go" && current?.supported && current.windows
          ? (Object.keys(windowNames) as (keyof typeof windowNames)[]).filter((key) => current.windows?.[key]).map((key) => {
              const window = current.windows?.[key];
              return <span className="context-row context-row--sub" key={key}>
                <span className="context-label">{windowNames[key]}</span>
                <span className="context-value" title={window?.resetsAt ? `重置时间 ${new Date(window.resetsAt).toLocaleString("zh-CN")}` : undefined}>{`剩余 ${window?.status === "rate-limited" ? 0 : Math.max(0, Math.round(100 - (window?.percent ?? 0)))}%`}</span>
              </span>;
            })
          : null}
        {!current?.supported && <span className="context-quota-notice">{current?.reason ?? "正在查询额度…"}</span>}
      </>}
    </span>
  </span>;
}
