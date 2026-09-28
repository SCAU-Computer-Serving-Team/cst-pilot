import { Button } from "@heroui/react";
import { useEffect, useState } from "react";
import { apiJson, type ProviderStatus } from "../app/api";
import { Sidebar } from "../app/shell";

type Theme = "light" | "dark" | "system";
type Model = { provider: string; id: string; name: string };
export default function Settings() {
  const [theme, setTheme] = useState<Theme>("system");
  const [providers, setProviders] = useState<ProviderStatus[]>([]);
  const [models, setModels] = useState<Model[]>([]);
  const [scope, setScope] = useState("");
  const [error, setError] = useState("");
  const [working, setWorking] = useState(false);
  async function refresh() {
    try {
      const [settings, auth, available] = await Promise.all([
        apiJson<{ theme: Theme }>("/api/settings"),
        apiJson<{ providers: ProviderStatus[] }>("/api/auth"),
        apiJson<{ models: Model[]; enabled: string[] }>("/api/models"),
      ]);
      setTheme(settings.theme); setProviders(auth.providers); setModels(available.models); setScope(available.enabled.join("\n")); setError("");
    } catch (cause) { setError(cause instanceof Error ? cause.message : "设置加载失败"); }
  }
  useEffect(() => { void refresh(); }, []);
  async function updateTheme(value: Theme) {
    setWorking(true);
    try {
      await apiJson("/api/settings", { method: "PATCH", body: { theme: value } });
      setTheme(value); document.documentElement.dataset.theme = value; setError("");
    } catch (cause) { setError(cause instanceof Error ? cause.message : "主题未保存"); }
    finally { setWorking(false); }
  }
  async function updateScope() {
    setWorking(true);
    try { await apiJson("/api/models/scoped", { method: "POST", body: { patterns: scope.split(/\r?\n/).map((value) => value.trim()).filter(Boolean) } }); setError(""); await refresh(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "模型范围未保存"); }
    finally { setWorking(false); }
  }
  async function logout(providerId: string) {
    setWorking(true);
    try { await apiJson(`/api/auth/${encodeURIComponent(providerId)}/logout`, { method: "POST" }); await refresh(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : "退出登录失败"); }
    finally { setWorking(false); }
  }
  return <div className="app-layout chat-layout"><Sidebar /><main className="chat-main settings-main">
    <header className="chat-header"><h1>设置</h1></header>
    <div className="settings-content">
      {error && <p role="alert" className="message-error">{error}</p>}
      <section><h2>外观</h2><p>主题会同步到本机页面。</p>
        <div className="settings-options" role="group" aria-label="页面主题">{(["system", "light", "dark"] as const).map((value) =>
          <Button key={value} variant={theme === value ? "primary" : "ghost"} isDisabled={working} onPress={() => void updateTheme(value)}>{({ system: "跟随系统", light: "浅色", dark: "深色" })[value]}</Button>
        )}</div>
      </section>
      <section><h2>模型服务</h2><p>凭据只在后端保存。需要重新登录时，请打开登录页更新 API KEY。</p>
        {providers.filter((provider) => provider.type || provider.requiresLogin).map((provider) => <div key={provider.id} className="settings-row"><span>{provider.name}</span><span>{provider.requiresLogin ? "需要重新登录" : "已登录"}</span>
          {provider.type && <Button variant="ghost" isDisabled={working} onPress={() => void logout(provider.id)}>退出</Button>}
        </div>)}
        <details className="settings-providers"><summary>查看全部模型服务（{providers.length}）</summary>
          {providers.filter((provider) => !provider.type && !provider.requiresLogin).map((provider) => <div className="settings-row" key={provider.id}><span>{provider.name}</span><span>未登录</span></div>)}
        </details>
        <a href="/login" className="settings-login-link">配置 API KEY</a>
      </section>
      <section><h2>启用模型范围</h2><p>共发现 {models.length} 个模型。每行填写一个模型范围；留空使用默认范围。</p>
        <textarea aria-label="启用模型范围" value={scope} onChange={(event) => setScope(event.target.value)} placeholder="provider/model-id" />
        <Button isDisabled={working} onPress={() => void updateScope()}>保存模型范围</Button>
      </section>
      <section><h2>快捷键</h2><p>Enter 发送，Shift + Enter 换行；运行时 Esc 停止当前会话。</p></section>
    </div>
  </main></div>;
}
