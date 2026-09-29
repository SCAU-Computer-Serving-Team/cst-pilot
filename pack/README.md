# 构建发行版

在项目根目录运行：

```powershell
node\node.exe pack\pack.mjs --official <官方ZIP或目录> --out <新的输出目录> --zip
```

脚本核对官方文件和扩展版本，在副本中用本机模拟模型测试，再按清单生成 ZIP 并解压校验。无需真实密钥；测试副本保留在输出目录的 `_smoke` 下，不进入 ZIP。

发行配置默认使用 OpenCode Go 的 `deepseek-flash`（DeepSeek V4.1 Flash）。打包时补全离线模型目录，不复制本机 `auth.json`；运行时由队员在本机登录。

`--esbuild <本地esbuild入口>` 可复用已安装的 0.25.10。未指定时使用固定版本的 npx。`--skip-smoke` 仅用于检查装配目录，不能同时生成 ZIP。

扩展依赖锁在 `extensions.package.json` 和 `extensions.package-lock.json`。新环境将它们分别复制到 `agent/home/npm/package.json`、`package-lock.json`，再运行 `npm ci --ignore-scripts --prefix agent/home/npm`。升级依赖时一起更新锁文件并重新验收。

发布输出目录中的 ZIP、VERSION 和 SHA256SUMS。不要再次压缩运行过的目录。自动冒烟只检查启动和工具注册，跨机诊断仍需按 `doc/test/README.md` 验收。
