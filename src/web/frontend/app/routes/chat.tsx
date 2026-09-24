import { Button } from "@heroui/react";
import { Brain, CheckCheck, ChevronDown, ChevronRight, Copy, Download, GitFork, Globe, HardDrive, Terminal } from "lucide-react";
import { useParams, useSearchParams } from "react-router";
import { Composer, Sidebar } from "../app/shell";

function PreviewConversation() {
  return (
    <div className="conversation">
      <div className="user-line"><p className="user-bubble">查看 C 盘的状况</p></div>
      <article className="assistant-block">
        <div className="conversation-status"><CheckCheck size={17} className="success-icon" /><span>已完成检查</span><small>2 次工具调用</small><ChevronDown size={14} /></div>
        <h2>C 盘剩余 40 GB，空间已用 86.5%</h2>
        <p>系统报告硬盘健康。剩余空间还够用，但建议关注持续增长的日志和缓存。</p>
        <table className="diagnostic-table"><thead><tr><th>检查项</th><th>结果</th><th>说明</th></tr></thead><tbody>
          <tr><td>C 盘容量</td><td>295.3 GB</td><td>剩余 40 GB，已用 86.5%</td></tr>
          <tr><td>硬盘型号</td><td>YMTC PC411-1TB-B</td><td>NVMe 固态硬盘</td></tr>
          <tr><td>健康状态</td><td className="health-value">Healthy</td><td>SMART 温度与寿命数据未获取</td></tr>
        </tbody></table>
        <ul><li>占用较多：Program Files 29.9 GB、ProgramData 19.6 GB。</li><li>目录扫描达到 50 万文件预算，以上为部分统计，不代表全部占用。</li></ul>
        <div className="answer-actions">
          <Button variant="ghost" isIconOnly isDisabled aria-label="复制回答尚未开放"><Copy size={15} /></Button>
          <Button variant="ghost" isIconOnly isDisabled aria-label="派生会话尚未开放"><GitFork size={15} /></Button>
          <Button variant="ghost" isIconOnly isDisabled aria-label="导出会话尚未开放"><Download size={15} /></Button>
          <span>19:35</span>
        </div>
      </article>
      <div className="user-line"><p className="user-bubble">帮我查这个固态的信息</p></div>
      <article className="assistant-block assistant-block--pending">
        <div className="conversation-status"><Brain size={18} /><span>已思考 · 确认查询范围</span><ChevronRight size={14} /></div>
        <div className="conversation-status"><Terminal size={18} className="accent-icon" /><span>正在调用多个工具…</span></div>
        <div className="search-status"><div className="conversation-status"><Globe size={18} className="accent-icon" /><span>正在联网检索…</span></div><small>YMTC PC411 1TB 规格 · PCIe Gen4 · TLC · 主机缓存 HMB</small></div>
      </article>
    </div>
  );
}

export default function Chat() {
  const { sessionId } = useParams();
  const [params] = useSearchParams();
  const preview = params.get("preview") === "1" && sessionId === "preview";
  return (
    <div className="app-layout chat-layout">
      <Sidebar preview={preview} />
      <main className="chat-main">
        <header className="chat-header"><HardDrive size={20} /><h1>{preview ? "C 盘空间与硬盘信息" : "会话"}</h1>{preview && <span className="preview-tag">设计预览 · 示例数据</span>}</header>
        {preview ? <PreviewConversation /> : <div className="chat-empty">会话接管将在下一阶段开放。</div>}
        <div className="chat-composer-area"><Composer /><div className="composer-footnote"><span>Enter 发送 · Shift + Enter 换行</span><span>诊断建议仅供参考，请结合设备实际情况核验。</span></div></div>
      </main>
    </div>
  );
}
