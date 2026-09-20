import { type CoreResult, type DeviceRow, parseDriverResult } from "./driver-data.ts";
import { collectionNotice, psString } from "./pwsh-data.ts";
import { createPwshRunner, PWSH } from "./runtime.ts";

/**
 * driver-core - 设备与驱动健康查询引擎（cst-pilot 定制，driver 工具的共享核心）
 *
 * 结构只读：WMI/CIM 纯查询，无任何写路径（不装驱动、不启停设备、不改服务）。
 * 设计：doc/design/tool/driver-design.md。
 *
 * 职责边界：
 * - 本文件：CMD 模板构造（拼接参数仅来自白名单字段）→ 执行解析 →
 *   scope 采集（problem/core/external/find）与路由（runScope）
 * - driver.ts：仅工具注册与 schema（薄壳），全部逻辑在此以便直连 harness 测试。
 *
 * 采集原则（设计定稿）：
 * - 数据全部来自 Win32_PnPEntity / Win32_NetworkAdapter / Win32_PnPSignedDriver /
 *   Win32_VideoController / Win32_DiskDrive / Win32_Service 的原生字段，
 *   一条查询出齐，不做逐设备二次查询
 * - 错误码（ConfigManagerErrorCode）原样透传，不翻译不养对照表；
 *   人话解读由模型联网查官方文档
 * - 过滤只用数值、枚举、固定前缀（USB\\ BTHENUM\\ DISPLAY\\）、固定类名，
 *   不匹配本地化字符串
 * - 没有蓝牙硬件、没插外设等均为合法数据，空数组不报错
 *
 * 实测结论（2026-09-03，pwsh 7.6.5，写入 harness tests/_t12.mjs）：
 * - Net 取 Win32_NetworkAdapter 时以 NetConnectionID IS NOT NULL 过滤：
 *   不加过滤会带出几十条 legacy 伪适配器（RAS/WDIS 历史条目），
 *   该过滤是结构性过滤（是否出现在网络连接面板），不是本地化过滤
 * - Win32_PnPSignedDriver 的 DriverDate 经 Get-CimInstance 已自动转为
 *   DateTime 对象，ToString('yyyy-MM-dd') 直接可用，无需手工解析 DMTF
 * - find 的 id 条件不做 WQL 下推（HardwareID 是字符串数组，WQL LIKE 不支持），
 *   改为全枚举 + Node 侧对 deviceId/hardwareIds 双通道后置匹配
 */

// 随包 pwsh 是产品契约（pi.cmd 严格 PATH 白名单 + zip 分发 pwsh\）。
// CST_PILOT_PWSH 仅供开发机注入系统 pwsh（本仓库 checkout 不含 pwsh\ 时做直连验证），产品环境不用。

const runPwsh = createPwshRunner({ timeoutMs: 30000, diagnostics: true, path: process.env.CST_PILOT_PWSH || PWSH });

/* ------------------------------------------------------------------ */
/* WQL 转义与白名单校验（注入防护：模型输入只进结构化字段）               */
/* ------------------------------------------------------------------ */

/** WQL LIKE 通配符转义：% _ [ \ → 字面量（单次扫描，替换互不嵌套污染）。
 *  \ 必须转义且只能翻倍成 \\（两个）：WQL LIKE 中反斜杠是转义前缀，
 *  PCI\VEN_8086 这类带 \ 的硬件 ID 不转义会静默失配返回 0
 *  （2026-09-04 真机测试踩坑；不能用字符组 [\\]，组内 \\] 会被当
 *  组内转义闭合歧义，pwsh 实测同样失配） */
export function escapeLike(s: string): string {
	return s.replace(/[[%_\\]/g, (ch) => (ch === "\\" ? "\\\\" : ch === "[" ? "[[]" : `[${ch}]`));
}

/** WQL 双引号字符串字面量转义：" → "" */
export function escapeWqlStr(s: string): string {
	return s.replace(/"/g, '""');
}

/** find.name：Node 侧字面子串匹配，可含任意可见字符，拒绝控制字符 */
const RE_NAME = /^[^\x00-\x1f]{1,100}$/;
/** find.class：固定英文类名（Net / MEDIA / Bluetooth / Display / USB 等） */
const RE_CLASS = /^[A-Za-z0-9][A-Za-z0-9 ._-]{0,39}$/;
/** find.id：硬件 ID / DeviceID 子串（USB\VID_xxxx&PID_xxxx... 形态） */
const RE_ID = /^[A-Za-z0-9][A-Za-z0-9 .\\_&()#,+-]{0,199}$/;

/* ------------------------------------------------------------------ */
/* CMD 模板（同 sys：模板串由代码构造，拼接参数仅来自白名单字段）          */
/* ------------------------------------------------------------------ */

/** problem：Status=Error/Unknown 的 PnP 设备（不用 ConfigManagerErrorCode!=0
 *  过滤——按状态过滤才能盖住 errorCode=0 的幽灵异常设备，指纹识别器有先例） */
export const CMD_PROBLEM = `
$ErrorActionPreference = 'SilentlyContinue'
$p = Get-CimInstance Win32_PnPEntity -Filter 'Status="Error" OR Status="Unknown"' |
  ForEach-Object { [ordered]@{ name = $_.Name; class = $_.PNPClass; status = $_.Status; errorCode = $_.ConfigManagerErrorCode; deviceId = $_.DeviceID; hardwareIds = @($_.HardwareID) } }
[ordered]@{ devices = @($p); count = @($p).Count } | ConvertTo-Json -Depth 4 -Compress
`;

/** external：DeviceID 前缀白名单（USB\ BTHENUM\ DISPLAY\）+ 可移动存储。
 *  USB\ 前缀 LIKE 天然连带 USBSTOR\（U 盘存储节点，外设排查需要它）；
 *  内置 USB 设备（自带摄像头）与内置屏同前缀，一并如实返回，由模型区分 */
export const CMD_EXTERNAL = `
$ErrorActionPreference = 'SilentlyContinue'
$d = Get-CimInstance Win32_PnPEntity -Filter "DeviceID LIKE 'USB%' OR DeviceID LIKE 'BTHENUM%' OR DeviceID LIKE 'DISPLAY%'" |
  ForEach-Object { [ordered]@{ name = $_.Name; class = $_.PNPClass; status = $_.Status; errorCode = $_.ConfigManagerErrorCode; deviceId = $_.DeviceID; hardwareIds = @($_.HardwareID) } }
$r = Get-CimInstance Win32_DiskDrive -Filter "InterfaceType='USB' OR MediaType LIKE 'Removable%'" |
  ForEach-Object { [ordered]@{ model = $_.Model; interface = $_.InterfaceType; mediaType = $_.MediaType; sizeGB = if ($_.Size) { [math]::Round($_.Size / 1GB, 1) } else { $null } } }
[ordered]@{ devices = @($d); removable = @($r) } | ConvertTo-Json -Depth 4 -Compress
`;

/** core：四类硬件现状 + bthserv/Audiosrv 服务状态 + 驱动版本日期表。
 *  Net 只取网络连接面板真实条目（NetConnectionID 非空，结构性过滤）；
 *  虚拟网卡不剔除——「虚拟网卡干扰」的判断前提是模型能看到虚拟网卡；
 *  显示复用 Win32_VideoController（与 sys gpu 同源） */
export const CMD_CORE = `
$ErrorActionPreference = 'SilentlyContinue'
$net = Get-CimInstance Win32_NetworkAdapter -Filter 'NetConnectionID IS NOT NULL' |
  ForEach-Object { [ordered]@{ name = $_.Name; connId = $_.NetConnectionID; physical = [bool]$_.PhysicalAdapter; connStatus = $_.NetConnectionStatus } }
$bt = Get-CimInstance Win32_PnPEntity -Filter "PNPClass='Bluetooth'" |
  ForEach-Object { [ordered]@{ name = $_.Name; status = $_.Status; errorCode = $_.ConfigManagerErrorCode } }
$audio = Get-CimInstance Win32_PnPEntity -Filter "PNPClass='MEDIA'" |
  ForEach-Object { [ordered]@{ name = $_.Name; status = $_.Status; errorCode = $_.ConfigManagerErrorCode } }
$disp = Get-CimInstance Win32_VideoController |
  ForEach-Object { [ordered]@{ name = $_.Name; vendor = $_.AdapterCompatibility; driver = $_.DriverVersion; status = $_.Status; bus = ($_.PNPDeviceID -split '\\\\')[0] } }
$svc = Get-CimInstance Win32_Service -Filter "Name='bthserv' OR Name='Audiosrv'" |
  ForEach-Object { [ordered]@{ name = $_.Name; state = $_.State } }
$drv = Get-CimInstance Win32_PnPSignedDriver -Filter "DeviceClass='NET' OR DeviceClass='MEDIA' OR DeviceClass='BLUETOOTH' OR DeviceClass='DISPLAY'" |
  ForEach-Object { [ordered]@{ class = $_.DeviceClass; device = $_.DeviceName; version = $_.DriverVersion; date = if ($_.DriverDate) { $_.DriverDate.ToString('yyyy-MM-dd') } else { $null }; provider = $_.DriverProviderName } }
[ordered]@{ net = @($net); bluetooth = @($bt); audio = @($audio); display = @($disp); services = @($svc); drivers = @($drv) } | ConvertTo-Json -Depth 4 -Compress
`;

export interface FindCond {
	name?: string; // 名称子串
	class?: string; // 设备类，固定英文类名精确匹配
	id?: string; // 硬件 ID / DeviceID 子串
}

/** find：class 白名单条件下推 WQL，过滤字符串经 psString 作为数据传入。
 *  name/id 在 Node 侧按字面子串匹配；HardwareID 为数组，不能用 DeviceID 预筛。 */
export function buildFindCmd(cond: FindCond): string {
	const parts: string[] = [];
	// Name and hardware IDs use literal Node-side substring matching.
	if (cond.class) parts.push(`PNPClass="${escapeWqlStr(cond.class)}"`);

	const wql = parts.join(" AND ");
	return `
$ErrorActionPreference = 'SilentlyContinue'
$d = Get-CimInstance Win32_PnPEntity ${wql ? `-Filter ${psString(wql)}` : ""} |
  ForEach-Object { [ordered]@{ name = $_.Name; class = $_.PNPClass; status = $_.Status; errorCode = $_.ConfigManagerErrorCode; deviceId = $_.DeviceID; hardwareIds = @($_.HardwareID) } }
[ordered]@{ devices = @($d); count = @($d).Count } | ConvertTo-Json -Depth 4 -Compress
`;
}

/* ------------------------------------------------------------------ */
/* 采集与收敛                                                           */
/* ------------------------------------------------------------------ */

/** 已知盲区（设计定稿，如实告知模型）：飞行模式/射频开关 WMI 读不到；
 *  网络打印机不走 PnP 枚举。设备全正常但功能异常时先排查这两处 */
const NOTICE_BLINDSPOT =
	"盲区：飞行模式/射频开关状态与网络打印机不在 WMI/CIM 能力内，设备全正常但功能异常时先排查这两处";

export async function collectProblem(signal?: AbortSignal): Promise<CoreResult> {
	signal?.throwIfAborted();
	const r = parseDriverResult(await runPwsh(CMD_PROBLEM, { signal }));
	if (r.error) return { error: r.error };
	return {
		devices: r.devices ?? [],
		count: r.count ?? 0,
		degraded: r.degraded,
		collectionErrors: r.collectionErrors,
		notice: `${collectionNotice(r)}errorCode 为 ConfigManagerErrorCode 原始值（0 = 正常），不翻译；hardwareIds（VEN/DEV）供联网定位驱动。${NOTICE_BLINDSPOT}`,
	};
}

export async function collectExternal(signal?: AbortSignal): Promise<CoreResult> {
	signal?.throwIfAborted();
	const r = parseDriverResult(await runPwsh(CMD_EXTERNAL, { signal }));
	if (r.error) return { error: r.error };
	return {
		devices: r.devices ?? [],
		removable: r.removable ?? [],
		degraded: r.degraded,
		collectionErrors: r.collectionErrors,
		notice: `${collectionNotice(r)}devices 含内置 USB 设备（自带摄像头）与内置屏（DISPLAY\\ 前缀），由调用方区分；errorCode 为原始值不翻译。${NOTICE_BLINDSPOT}`,
	};
}

export async function collectCore(signal?: AbortSignal): Promise<CoreResult> {
	signal?.throwIfAborted();
	const r = parseDriverResult(await runPwsh(CMD_CORE, { signal }));
	if (r.error) return { error: r.error };
	return {
		net: r.net ?? [],
		bluetooth: r.bluetooth ?? [],
		audio: r.audio ?? [],
		display: r.display ?? [],
		services: r.services ?? [],
		drivers: r.drivers ?? [],
		degraded: r.degraded,
		collectionErrors: r.collectionErrors,
		notice: `${collectionNotice(r)}services = bthserv / Audiosrv 状态（缺失 = 无该服务）；connStatus 为 NetConnectionStatus 原始值；drivers 表驱动是否过旧请联网比对最新版，工具不判断。${NOTICE_BLINDSPOT}`,
	};
}

/** find：条件 AND 组合，至少传一个。id 双通道匹配（deviceId 或 hardwareIds
 *  任一包含子串，忽略大小写）——WQL LIKE 不支持数组属性，无法下推 */
export async function collectFind(raw: FindCond, signal?: AbortSignal): Promise<CoreResult> {
	signal?.throwIfAborted();
	const name = typeof raw.name === "string" ? raw.name.trim() : undefined;
	const klass = typeof raw.class === "string" ? raw.class.trim() : undefined;
	const id = typeof raw.id === "string" ? raw.id.trim() : undefined;
	if (!name && !klass && !id) {
		return { error: "find 需要至少一个条件：name（名称子串）/ class（设备类精确名）/ id（硬件 ID 子串）" };
	}
	if (name && !RE_NAME.test(name)) return { error: "name 含非法字符或超长（≤100 字符）" };
	if (klass && !RE_CLASS.test(klass)) return { error: "class 只接受固定英文类名（≤40 字符，字母数字 ._ -）" };
	if (id && !RE_ID.test(id)) return { error: "id 含非法字符或超长（≤200 字符）" };

	const r = parseDriverResult(await runPwsh(buildFindCmd({ name, class: klass, id }), { signal }));
	if (r.error) return { error: r.error };
	let devices: DeviceRow[] = r.devices ?? [];
	if (name) devices = devices.filter((d) => (d.name ?? "").toLowerCase().includes(name.toLowerCase()));
	if (id) {
		const needle = id.toLowerCase();
		devices = devices.filter(
			(d) =>
				(d.deviceId ?? "").toLowerCase().includes(needle) ||
				(d.hardwareIds ?? []).some((h) => (h ?? "").toLowerCase().includes(needle)),
		);
	}
	return {
		devices,
		count: devices.length,
		degraded: r.degraded,
		collectionErrors: r.collectionErrors,
		notice: `${collectionNotice(r)}errorCode 为原始值不翻译；匹配条件 AND 组合，id 对 deviceId 与 hardwareIds 双通道匹配。`,
	};
}

/* ------------------------------------------------------------------ */
/* scope 路由                                                           */
/* ------------------------------------------------------------------ */

export const SCOPES = ["problem", "core", "external", "find"] as const;

export async function runScope(raw: Record<string, unknown>, signal?: AbortSignal): Promise<CoreResult> {
	signal?.throwIfAborted();
	const scope = typeof raw?.scope === "string" ? raw.scope : "problem";
	switch (scope) {
		case "problem":
			return collectProblem(signal);
		case "core":
			return collectCore(signal);
		case "external":
			return collectExternal(signal);
		case "find":
			return collectFind(
				{
					name: typeof raw.name === "string" ? raw.name : undefined,
					class: typeof raw.class === "string" ? raw.class : undefined,
					id: typeof raw.id === "string" ? raw.id : undefined,
				},
				signal,
			);
		default:
			return { error: `未知 scope: ${scope}（当前支持 ${SCOPES.join(" / ")}）` };
	}
}
