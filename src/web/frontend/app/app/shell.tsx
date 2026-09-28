import { Button, Popover } from "@heroui/react";
import { LogIn, LogOut, Monitor, PanelLeft, Settings, SquarePen, MessageCircleQuestion, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router";
import { apiJson, type ProviderStatus } from "./api";
import { BlueHour } from "./blue-hour";
import { Composer, type ComposerConfig } from "./composer";
import { useSessions } from "./web-state";

export const sampleSessions = [
  { id: "preview", title: "C 盘空间与硬盘信息", group: "今天" },
  { id: "fan-preview", title: "风扇狂转还降频", group: "今天" },
  { id: "startup-preview", title: "开机要等三分钟", group: "昨天" },
];

// 画布按日期分组会话列表。
const sessionGroups = ["今天", "昨天", "更早"];
const dayGroup = (value?: string) => {
  const target = value ? new Date(value) : undefined;
  if (!target || Number.isNaN(target.getTime())) return "更早";
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const day = new Date(target);
  day.setHours(0, 0, 0, 0);
  const days = Math.round((today.getTime() - day.getTime()) / 86400000);
  return days <= 0 ? "今天" : days === 1 ? "昨天" : "更早";
};

export function Sidebar({ preview = false }: { preview?: boolean }) {
  const location = useLocation();
  const state = useSessions();
  const sessions: { id: string; title: string; group?: string; updatedAt?: string }[] = preview ? sampleSessions : state.sessions;
  const groupOf = (session: { group?: string; updatedAt?: string }) => preview ? session.group ?? "今天" : dayGroup(session.updatedAt);
  const navigate = useNavigate();
  const [providers, setProviders] = useState<ProviderStatus[]>([]);
  const [accountError, setAccountError] = useState("");
  const refreshAccount = useCallback(async () => {
    if (preview) return;
    try { const data = await apiJson<{ providers: ProviderStatus[] }>("/api/auth"); setProviders(data.providers); setAccountError(""); }
    catch (cause) { setAccountError(cause instanceof Error ? cause.message : "无法读取登录状态"); }
  }, [preview]);
  useEffect(() => {
    void refreshAccount();
    if (preview) return;
    const events = new EventSource("/api/events");
    events.addEventListener("state", () => { void refreshAccount(); });
    events.addEventListener("reset", () => { void refreshAccount(); });
    return () => events.close();
  }, [preview, refreshAccount]);
  async function logout(id: string) {
    try {
      await apiJson(`/api/auth/${encodeURIComponent(id)}/logout`, { method: "POST" });
      await refreshAccount();
    } catch (cause) { setAccountError(cause instanceof Error ? cause.message : "退出登录失败"); }
  }
  async function remove(id: string) {
    if (!window.confirm("删除此会话及其图片？此操作无法撤销。")) return;
    try {
      await apiJson(`/api/sessions/${encodeURIComponent(id)}`, { method: "DELETE" });
      if (location.pathname.startsWith(`/s/${id}`)) navigate("/");
      await state.refresh();
    } catch (cause) { window.alert(cause instanceof Error ? cause.message : "删除失败"); }
  }
  return (
    <aside className="sidebar" aria-label="会话导航">
      <div className="sidebar-top">
        <div className="sidebar-brand">
          <Link className="product-name" to={preview ? "/?preview=1" : "/"}>CST Pilot</Link>
          <Button variant="ghost" isIconOnly isDisabled className="sidebar-icon" aria-label="会话搜索尚未开放"><PanelLeft size={22} /></Button>
        </div>
        <Link className={`sidebar-new ${location.pathname === "/" ? "selected" : ""}`} to={preview ? "/?preview=1" : "/"}>
          <SquarePen size={18} />新对话
        </Link>
        {sessions.length > 0 ? sessionGroups.map((group) => {
          const list = sessions.filter((session) => groupOf(session) === group);
          return list.length === 0 ? null : <section className="sidebar-group" key={group} aria-label={group}>
            <h2>{group}</h2>
            {list.map((session) => (
              <div className="sidebar-session-row" key={session.id}>
                <Link className={`sidebar-session ${location.pathname === `/s/${session.id}` ? "selected" : ""}`}
                  to={`/s/${session.id}${preview ? "?preview=1" : ""}`}>
                  <span>{session.title}</span>
                </Link>
                {!preview && <Button variant="ghost" isIconOnly className="sidebar-remove" aria-label={`删除会话 ${session.title}`} onPress={() => void remove(session.id)}><Trash2 size={15} /></Button>}
              </div>
            ))}
          </section>;
        }) : <p className="sidebar-empty">暂无会话</p>}
        {state.error && <p className="sidebar-error" role="alert">{state.error}</p>}
      </div>
      <div className="sidebar-footer">
        <div className="device-icon"><Monitor size={18} /></div>
        <div className="device-copy"><span>{preview ? "Tim2354" : "当前电脑"}</span><small>{preview ? "24宣传指导 · 设计预览" : state.stage === "preview" ? "页面预览模式" : "本机运行中"}</small></div>
        <Popover>
          <Button variant="ghost" isIconOnly className="sidebar-account-trigger" aria-label="账号菜单"><Settings size={22} aria-hidden="true" /></Button>
          <Popover.Content placement="top end" className="sidebar-account-popover">
            <Popover.Dialog aria-label="账号菜单" className="sidebar-account-menu">
              {providers.filter((provider) => provider.type).map((provider) => <div className="sidebar-auth-row" key={provider.id}>
                <span title={provider.name}>{provider.name}{provider.requiresLogin ? " · 需重新登录" : " · 已登录"}</span>
                <Button variant="ghost" isIconOnly aria-label={`退出 ${provider.name}`} onPress={() => void logout(provider.id)}><LogOut size={16} /></Button>
              </div>)}
              <Link className="sidebar-account-item" to="/login"><LogIn size={18} aria-hidden="true" />{providers.some((provider) => provider.type) ? "管理登录" : "登录"}</Link>
              <Link className="sidebar-account-item" to="/settings"><Settings size={18} aria-hidden="true" />设置</Link>
              {accountError && <p role="alert" className="sidebar-error">{accountError}</p>}
              <Button variant="ghost" isDisabled className="sidebar-account-item" aria-label="反馈问题功能尚未开放"><MessageCircleQuestion size={18} aria-hidden="true" />反馈问题</Button>
            </Popover.Dialog>
          </Popover.Content>
        </Popover>
      </div>
    </aside>
  );
}

export function BrandArcs() {
  return (
    <svg className="brand-arcs" viewBox="0 0 400 400" fill="none" aria-hidden="true">
      <defs><linearGradient id="brand-arc-gradient" x1="0%" y1="100%" x2="0%" y2="0%">
        <stop stopColor="#fff" stopOpacity="0" />
        <stop offset="55%" stopColor="#fff" stopOpacity=".12" />
        <stop offset="100%" stopColor="#fff" />
      </linearGradient></defs>
      <path d="M200 200 L340 60 A197.5 197.5 0 1 0 60 340 Z" stroke="url(#brand-arc-gradient)" strokeWidth="5" />
      <path d="M200 200 L323 77 A171.5 171.5 0 1 0 77 323 Z" stroke="url(#brand-arc-gradient)" strokeWidth="5" />
    </svg>
  );
}

export function HomeSurface({ preview = false }: { preview?: boolean }) {
  const navigate = useNavigate();
  async function send(text: string, config: ComposerConfig) {
    const session = await apiJson<{ id: string }>("/api/sessions", { method: "POST", body: config.provider ? { provider: config.provider, modelId: config.modelId, thinkingLevel: config.thinkingLevel } : {}, idempotencyKey: config.messageId });
    await apiJson(`/api/sessions/${encodeURIComponent(session.id)}/messages`, { method: "POST", idempotencyKey: config.messageId, body: { id: config.messageId, text, delivery: "queue", images: config.images, ...(config.skill ? { skill: config.skill } : {}) } });
    navigate(`/s/${session.id}`);
  }
  return (
    <div className="app-layout home-layout">
      <Sidebar preview={preview} />
      <main className="home-main">
        <BlueHour kind="home" />
        <div className="home-center">
          <div className="home-greeting"><BrandArcs /><h1>我该如何协助您？</h1></div>
          <Composer home onSend={preview ? undefined : send} />
        </div>
        <span className="home-footnote">@cst-pilot-web</span>
      </main>
    </div>
  );
}
