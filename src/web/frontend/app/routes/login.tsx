import { Button } from "@heroui/react";
import { Link } from "react-router";
import { BlueHour } from "../app/blue-hour";
import { HomeSurface } from "../app/shell";

export default function Login() {
  return (
    <div className="login-scene">
      <div className="login-behind" aria-hidden="true" inert><HomeSurface /></div>
      <div className="login-shade" aria-hidden="true" />
      <main className="login-panel" aria-label="登录">
        <BlueHour kind="air" />
        <div className="login-bottom-gradient" aria-hidden="true" />
        <Link className="login-brand" to="/">CST Pilot</Link>
        <div className="login-heading"><h1>欢迎回来</h1><p>登录以同步模型配置与额度</p></div>
        <div className="login-card">
          <div className="login-switch"><Button isDisabled className="login-switch-item selected">API KEY</Button></div>
          <p className="login-pending">API KEY 登录画布待补，暂不能登录。</p>
        </div>
        <span className="login-footnote">@cst-pilot-web</span>
      </main>
    </div>
  );
}
