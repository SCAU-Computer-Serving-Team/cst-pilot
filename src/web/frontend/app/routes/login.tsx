import { Button, Checkbox, Input, Label, ListBox, Select, TextField } from "@heroui/react";
import { Check, ChevronDown, Eye, EyeOff } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import { BlueHour } from "../app/blue-hour";
import { HomeSurface } from "../app/shell";

const providers = ["CST", "OpenAI", "Anthropic", "Google", "DeepSeek"];

export default function Login() {
  const [provider, setProvider] = useState("CST");
  const [customProvider, setCustomProvider] = useState(false);
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);

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
          <div className="login-switch" aria-label="登录方式"><span className="login-switch-inactive">扫码登录</span><span className="login-switch-active">APIKEY</span></div>
          <div className="login-fields">
            <Select selectedKey={provider} onSelectionChange={(key) => { if (key != null) setProvider(String(key)); }} className="login-field login-provider">
              <Label>Provider</Label>
              <Select.Trigger className="login-input"><Select.Value>{provider}</Select.Value><ChevronDown size={16} aria-hidden="true" /></Select.Trigger>
              <Select.Popover className="login-provider-popover" placement="bottom start">
                <ListBox className="login-provider-list">
                  {providers.map((name) => <ListBox.Item key={name} id={name} textValue={name} className="login-provider-option">{name}<span className="login-provider-check"><Check size={16} aria-hidden="true" /></span></ListBox.Item>)}
                </ListBox>
              </Select.Popover>
            </Select>
            <Checkbox isSelected={customProvider} onChange={setCustomProvider} className="login-custom-checkbox">
              <Checkbox.Content>
                <Checkbox.Control><Checkbox.Indicator><Check size={13} aria-hidden="true" /></Checkbox.Indicator></Checkbox.Control>
                <Label>自定义 Provider</Label>
              </Checkbox.Content>
            </Checkbox>
            {customProvider && <TextField className="login-field" value={baseUrl} onChange={setBaseUrl}>
              <Label>BaseURL</Label><Input className="login-input" type="url" placeholder="https://api.example.com/v1" autoComplete="url" />
            </TextField>}
            <TextField className="login-field" value={apiKey} onChange={setApiKey}>
              <Label>APIKEY</Label>
              <div className="login-key-field"><Input className="login-input" type={showKey ? "text" : "password"} placeholder="输入 APIKEY" autoComplete="off" spellCheck={false} />
                <Button variant="ghost" isIconOnly className="login-key-visibility" onPress={() => setShowKey((visible) => !visible)} aria-label={showKey ? "隐藏 APIKEY" : "显示 APIKEY"}>{showKey ? <EyeOff size={16} /> : <Eye size={16} />}</Button>
              </div>
            </TextField>
          </div>
          <Button isDisabled className="login-submit" aria-label="登录功能开发中，暂不能提交">登录</Button>
        </div>
        <span className="login-footnote">@cst-pilot-web</span>
      </main>
    </div>
  );
}
