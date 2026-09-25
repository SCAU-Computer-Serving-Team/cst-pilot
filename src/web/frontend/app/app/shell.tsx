import { Button, Popover, TextArea } from "@heroui/react";
import { ChevronDown, LogIn, Monitor, PanelLeft, Plus, Settings, SquarePen, MessageSquare, ArrowUp, MessageCircleQuestion } from "lucide-react";
import { Link, useLocation } from "react-router";
import { BlueHour } from "./blue-hour";

export const sampleSessions = [
  { id: "preview", title: "C 盘空间与硬盘信息", group: "今天" },
  { id: "fan-preview", title: "风扇狂转还降频", group: "今天" },
  { id: "startup-preview", title: "开机要等三分钟", group: "昨天" },
];

export function Sidebar({ preview = false }: { preview?: boolean }) {
  const location = useLocation();
  const sessions = preview ? sampleSessions : [];
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
        {sessions.length > 0 ? ["今天", "昨天"].map((group) => (
          <section className="sidebar-group" key={group} aria-label={group}>
            <h2>{group}</h2>
            {sessions.filter((session) => session.group === group).map((session) => (
              <Link
                key={session.id}
                className={`sidebar-session ${location.pathname === `/s/${session.id}` ? "selected" : ""}`}
                to={`/s/${session.id}?preview=1`}
              >
                <MessageSquare size={18} aria-hidden="true" /><span>{session.title}</span>
              </Link>
            ))}
          </section>
        )) : <p className="sidebar-empty">暂无会话</p>}
      </div>
      <div className="sidebar-footer">
        <div className="device-icon"><Monitor size={18} /></div>
        <div className="device-copy"><span>{preview ? "Tim2354" : "当前电脑"}</span><small>{preview ? "24宣传指导 · 设计预览" : "会话功能准备中"}</small></div>
        <Popover>
          <Button variant="ghost" isIconOnly className="sidebar-account-trigger" aria-label="账号菜单"><Settings size={22} aria-hidden="true" /></Button>
          <Popover.Content placement="top end" className="sidebar-account-popover">
            <Popover.Dialog aria-label="账号菜单" className="sidebar-account-menu">
              <Link className="sidebar-account-item" to="/login"><LogIn size={18} aria-hidden="true" />登录</Link>
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

export function Composer({ home = false }: { home?: boolean }) {
  return (
    <div className={`composer ${home ? "composer--home" : "composer--chat"}`} aria-label="消息编辑器预览">
      <TextArea
        aria-label="消息"
        className="composer-input"
        disabled
        placeholder={home ? "描述这台电脑遇到的问题…" : "继续提问，或补充这台电脑的情况…"}
        rows={home ? 1 : 2}
      />
      <div className="composer-actions">
        <Button variant="ghost" isIconOnly isDisabled className="composer-icon" aria-label="附件功能尚未开放"><Plus size={20} /></Button>
        <span className="composer-spacer" />
        <Button variant="ghost" isDisabled className="composer-model">{home ? "deepseek-v4.1-flash" : "模型"}<ChevronDown size={13} /></Button>
        {!home && <Button variant="ghost" isDisabled className="composer-thinking">思考强度<ChevronDown size={12} /></Button>}
        <Button isIconOnly isDisabled className="composer-send" aria-label="会话功能尚未开放"><ArrowUp size={16} /></Button>
      </div>
    </div>
  );
}

export function HomeSurface({ preview = false }: { preview?: boolean }) {
  return (
    <div className="app-layout home-layout">
      <Sidebar preview={preview} />
      <main className="home-main">
        <BlueHour kind="home" />
        <div className="home-center">
          <div className="home-greeting"><BrandArcs /><h1>我该如何协助您？</h1></div>
          <Composer home />
        </div>
        <span className="home-footnote">@cst-pilot-web</span>
      </main>
    </div>
  );
}
