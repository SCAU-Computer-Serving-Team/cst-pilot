# 遥测 TLS 测试证书

本目录仅供 `telemetry_network.rs` 的本机 TLS 测试使用。

- `ca.cert.pem`：测试专用 CA 公钥证书。
- `server.cert.pem`、`server.key.pem`：测试服务证书与公开的测试私钥，仅匹配 `127.0.0.1`。
- CA 私钥不随仓库保存。这些证书不用于发行或生产服务。

测试显式传入 CA，不安装到系统证书库。有效期到 2126 年，过期后重新生成整套测试证书。
