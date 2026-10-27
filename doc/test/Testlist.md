# 环境覆盖要求

支持范围：**Windows 10/11 x64**。不覆盖 32 位 Windows、Windows 8 及以下、Linux 或 macOS。PE 单独验证运行与降级，不等于已承诺完整支持。

本页定义选样范围，不记录验收状态。执行方法见[测试指南](README.md)；同一台机器可以覆盖多个维度，不要求穷举全部组合。具体版本和结果保存在仓库外当天临时目录。

## 设备形态

差异项：[env-desktop](env-desktop.md)。

- 组装机
- 品牌台式机：联想、戴尔、惠普、方正
- 笔记本：华硕、联想、机械革命、惠普，优先双显卡
- 一体机
- 老旧机器：i3/i5 第 4–7 代、机械盘、Windows 10

## 操作系统

差异项：[env-os](env-os.md)。

- Windows 11 家庭版 / 专业版
- Windows 10 家庭版 / 专业版：1507–22H2 各版本覆盖情况分别记录
- Windows 10 政府专用或政采裁剪镜像：含安全软件、税务 UKey、CA Key、国密卡场景

## 工具包所在介质

差异项：[env-media](env-media.md)。介质与文件系统分别记录。

- USB 2.0 U 盘：低速基线
- USB 3.0 / 3.2 U 盘
- USB 3.0 移动机械硬盘
- 移动固态硬盘 PSSD
- SD 卡与读卡器
- FAT32：关注单文件小于 4 GiB 的限制，核对当前发行包文件
- exFAT
- NTFS

## PE

差异项：[env-pe](env-pe.md)。记录内核版本、组件与权限。

- 微 PE（WePE）
- FirPE
- 优启通（EPT）
- 其他第三方 PE
- 官方 ADK WinPE：未额外注入驱动的基线
