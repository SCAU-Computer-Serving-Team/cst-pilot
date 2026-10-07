import { goFlash } from "./model-catalog.mjs";

// 发行版 settings.json 的唯一来源：pack.mjs 写入发行树，测试直接断言这里的取值。
export const RELEASE_SETTINGS = {
  _comment:
    "默认开放读取、检索和诊断工具。扩展随包提供，启动不安装或更新。配置与会话保存在 agent/home，临时状态保存在 .state。此配置不是系统沙箱；PowerShell 等原生组件可能留下宿主缓存。",
  defaultProvider: "opencode-go",
  defaultModel: goFlash.id,
  defaultTools: ["read", "ls"],
  defaultProjectTrust: "never",
  enableInstallTelemetry: false,
  lastChangelogVersion: "0.85.1",
  theme: "light",
  packages: ["./packages/pi-fff", "./packages/pi-open-tui", "./packages/pi-web-access"],
};

// 项目遥测与 pi 自带 install telemetry 是两条独立链路；后者始终关闭。
export const RELEASE_TELEMETRY = {
  enabled: true,
  endpoint: "https://www.cstoa.top/api/telemetry",
  authProvider: "cstoa",
};
