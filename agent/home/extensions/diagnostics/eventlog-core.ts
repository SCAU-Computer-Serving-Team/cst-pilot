import { psString } from "./pwsh-data.ts";
import { asRecord, asRecords, createPwshRunner, errorMessage, PWSH } from "./runtime.ts";

/**
 * eventlog-core - 事件日志查询引擎（cst-pilot 定制，eventlog 工具的共享核心）
 *
 * 结构只读：Get-WinEvent 纯查询，无任何写路径。
 * 设计：doc/design/tool/event-design.md（里程碑 1-2 = 本文件）。
 *
 * 职责边界：
 * - 本文件：参数白名单校验 → pwsh 命令串构建（FilterHashtable 下推 + 多组 OR）→
 *   执行与解析 → 体积收敛（top 截断 / 来源·ID 折叠计数表 / 简述 200 字符）→
 *   scope 路由（runScope，含各 scope 白名单与降级逻辑）+ 单条详情（queryDetail）
 * - eventlog.ts：仅工具注册与 schema（薄壳），全部逻辑在此以便直连 harness 测试。
 *
 * 实测结论（2026-09-03，pwsh 7.6.5，写入 harness tests/_t9.mjs）：
 * - FilterHashtable 下推 LogName/Level/StartTime/Id/ProviderName 全部可用；
 *   Level 接受数组：warn 档 = @(1,2,3)（Critical/Error/Warning），
 *   error 档 = @(1,2)。结果默认时间倒序（newest-first）。
 * - 零命中：FullyQualifiedErrorId = NoMatchingEventsFound（语言无关），
 *   SilentlyContinue 下也会写入 $Error —— 以此区分「没事件」与「真出错」。
 *   注意：provider 条件匹配不到时是另一组 FQID（NoMatchingProvidersFound /
 *   LogsAndProvidersDontOverlap，各伴生一条 EventLogException），同样属于
 *   条件级零命中而非故障，需一并吸收（否则合法空结果会被误判为 error）。
 * - 通道不存在：FullyQualifiedErrorId = NoMatchingLogsFound（语言无关）。
 * - 毒事件（provider 消息资源损坏，坏机器常见）：异常类型 EventLogException。
 *   SilentlyContinue 下该记录被跳过、其余照常返回；ErrorActionPreference=Stop
 *   会把整条查询炸掉 —— 因此枚举必须 SilentlyContinue，并用异常类型分类 $Error。
 * - 体积实测：24h warn+err（System+Application）117 条 402ms；
 *   30d 全级别 4218 条 4.5s（本机健康机器，坏机器更慢 → 超时取 30s）。
 *
 * 里程碑 3-8 追加实测（2026-09-03）：
 * - SCM 提供者全表交叉核对（ListProvider，声明 ID 以 0xC0000000+N 等高位编码，
 *   解码后与 service 白名单逐条吻合，全部 Error 级；7025 在本机表未声明，按设计保留）
 * - WHEA 提供者精确名 = Microsoft-Windows-WHEA-Logger（下推必须用全名），
 *   且声明含 ID 19 —— 与 boot 白名单的 19（WindowsUpdateClient）重叠，
 *   boot 多组查询的去重（$seen 按 logName/recordId）是真实需要的
 * - Security 非管理员行为：FilterHashtable 形态下静默返回 0 条（NoMatchingEventsFound），
 *   直接 -LogName 才报 UnauthorizedAccessException —— security scope 必须显式
 *   admin 预检降级，不能依赖错误分类，否则「没权限」会伪装成「没有登录事件」
 * - 单条直取：XPath 元素名是 EventRecordID（PS 属性叫 RecordId，XML 元素不是），
 *   模板写死、仅插入整数校验后的 recordId —— 与 FilterHashtable 同等安全模型
 * - provider 后置正则用 [regex]::new + 1s MatchTimeout（防灾难性回溯）；
 *   消息子串用 IndexOf OrdinalIgnoreCase（无正则，零回溯风险）
 */

// 随包 pwsh 是产品契约（pi.cmd 严格 PATH 白名单 + zip 分发 pwsh\）。
// CST_PILOT_PWSH 仅供开发机注入系统 pwsh（本仓库 checkout 不含 pwsh\ 时做直连验证），产品环境不用。

const runPwsh = createPwshRunner({ timeoutMs: 30000, path: process.env.CST_PILOT_PWSH || PWSH });

/* ------------------------------------------------------------------ */
/* 参数白名单校验（注入防护：模型输入只进结构化字段，逐个白名单校验）      */
/* ------------------------------------------------------------------ */

/** 事件通道名：'System'、'Application'、'Microsoft-Windows-PowerShell/Operational' 等。
 *  只放行字母数字与 . / _ - 空格，引号、分号、$、{} 等一律拒绝。 */
const RE_LOGNAME = /^[A-Za-z0-9][A-Za-z0-9 ./_-]{0,99}$/;
/** 提供程序精确名（FilterHashtable ProviderName 不支持通配符，走下推的必须是精确名）：
 *  'Service Control Manager'、'Microsoft-Windows-WHEA-Logger' 等。 */
const RE_PROVIDER = /^[A-Za-z0-9.][A-Za-z0-9 ._-]{0,127}$/;

export type LevelTier = "warn" | "error";

export interface CoreQuery {
	logNames: string[]; // 查询通道（scope 层写死或经校验）
	hours: number; // 时间窗（小时）
	top: number; // 事件列表条数上限
	level?: LevelTier; // 最低级别档：warn=Warning 及更严重，error=Error 及更严重
	ids?: number[]; // 事件 ID 白名单（下推；0 为合法 ID，实测 hcmon 等提供者发 0）
	providers?: string[]; // 提供程序精确名（下推）
	msgLike?: string; // 后置过滤：消息子串（不区分大小写 IndexOf，无正则回溯风险）
	providerRe?: string; // 后置过滤：提供程序正则（.NET regex，1s 超时）
}

function asInt(v: unknown): number | null {
	if (typeof v === "number" && Number.isInteger(v)) return v;
	if (typeof v === "string" && /^-?\d+$/.test(v.trim())) return parseInt(v.trim(), 10);
	return null;
}

function dedupe(arr: string[]): string[] {
	return [...new Set(arr)];
}

/**
 * 校验并归一化查询参数。数字越界夹紧（无注入面），字符串越界直接拒绝（注入面）。
 * 违规抛 Error，文案模型可读，scope 层可直接透传。
 */
export function buildSpec(raw: {
	logNames?: unknown;
	hours?: unknown;
	top?: unknown;
	level?: unknown;
	ids?: unknown;
	providers?: unknown;
	msgLike?: unknown;
	providerRe?: unknown;
}): CoreQuery {
	// logNames：必填，字符串数组（元素必须 string：白名单字段的注入面一律拒绝而非强转）
	if (!Array.isArray(raw.logNames) || raw.logNames.length === 0) {
		throw new Error('logNames 必须是非空数组，如 ["System"]');
	}
	for (const x of raw.logNames) {
		if (typeof x !== "string") {
			throw new Error(`非法通道名: ${JSON.stringify(x)}（通道名必须是字符串）`);
		}
	}
	const logNames = dedupe(raw.logNames as string[]);
	for (const ln of logNames) {
		if (!RE_LOGNAME.test(ln)) {
			throw new Error(`非法通道名: ${JSON.stringify(ln)}（只接受字母数字与 . / _ - 空格）`);
		}
	}

	// hours：默认 24，夹紧 1..720（30 天）
	let hours = asInt(raw.hours) ?? 24;
	hours = Math.max(1, Math.min(720, hours));

	// top：默认 100，夹紧 1..100（设计硬规则：100 条上限）
	let top = asInt(raw.top) ?? 100;
	top = Math.max(1, Math.min(100, top));

	// level：枚举，可省略（省略 = 全级别）
	let level: LevelTier | undefined;
	if (raw.level !== undefined && raw.level !== null) {
		if (raw.level !== "warn" && raw.level !== "error") {
			throw new Error(`非法 level: ${JSON.stringify(raw.level)}（只接受 "warn" / "error"）`);
		}
		level = raw.level;
	}

	// ids / providers / msgLike / providerRe：白名单校验在 finalize 统一做
	return finalize({
		logNames,
		hours,
		top,
		level,
		ids: raw.ids,
		providers: raw.providers,
		msgLike: raw.msgLike,
		providerRe: raw.providerRe,
	});
}

function finalize(
	q: Omit<CoreQuery, "ids" | "providers" | "msgLike" | "providerRe"> & {
		ids?: unknown;
		providers?: unknown;
		msgLike?: unknown;
		providerRe?: unknown;
	},
): CoreQuery {
	let ids: number[] | undefined;
	if (q.ids !== undefined && q.ids !== null) {
		if (!Array.isArray(q.ids)) throw new Error("ids 必须是数字数组，如 [41, 6008]");
		const arr: number[] = [];
		for (const x of q.ids) {
			const n = asInt(x);
			if (n === null || n < 0 || n > 65535) {
				throw new Error(`非法事件 ID: ${JSON.stringify(x)}（只接受 0-65535 的整数）`);
			}
			arr.push(n);
		}
		ids = [...new Set(arr)];
	}

	let providers: string[] | undefined;
	if (q.providers !== undefined && q.providers !== null) {
		if (!Array.isArray(q.providers)) throw new Error("providers 必须是字符串数组");
		for (const x of q.providers) {
			if (typeof x !== "string") {
				throw new Error(`非法提供程序名: ${JSON.stringify(x)}（提供程序名必须是字符串）`);
			}
		}
		const arr = dedupe(q.providers as string[]);
		for (const p of arr) {
			if (!RE_PROVIDER.test(p)) {
				throw new Error(`非法提供程序名: ${JSON.stringify(p)}（下推只接受精确名：字母数字与 . _ - 空格）`);
			}
		}
		providers = arr;
	}

	let msgLike: string | undefined;
	if (q.msgLike !== undefined && q.msgLike !== null) {
		if (typeof q.msgLike !== "string" || q.msgLike.length < 1 || q.msgLike.length > 200) {
			throw new Error(`非法消息过滤词: ${JSON.stringify(q.msgLike)}（只接受 1-200 字符的子串）`);
		}
		msgLike = q.msgLike;
	}

	let providerRe: string | undefined;
	if (q.providerRe !== undefined && q.providerRe !== null) {
		if (typeof q.providerRe !== "string" || q.providerRe.length < 1 || q.providerRe.length > 128) {
			throw new Error(`非法 provider 正则: ${JSON.stringify(q.providerRe)}（只接受 1-128 字符）`);
		}
		try {
			new RegExp(q.providerRe); // 语法烟测（.NET 侧编译失败还有结构化上报兜底）
		} catch {
			throw new Error(`非法 provider 正则: ${JSON.stringify(q.providerRe)}（语法无法编译）`);
		}
		providerRe = q.providerRe;
	}

	const out: CoreQuery = { logNames: q.logNames, hours: q.hours, top: q.top };
	if (q.level) out.level = q.level;
	if (ids?.length) out.ids = ids;
	if (providers?.length) out.providers = providers;
	if (msgLike) out.msgLike = msgLike;
	if (providerRe) out.providerRe = providerRe;
	return out;
}

/* ------------------------------------------------------------------ */
/* 命令串构建：FilterHashtable 下推（LogName/Level/StartTime/Id/ProviderName） */
/* ------------------------------------------------------------------ */

/** 简述上限（字符）：与 pwsh 侧一致，全文留给 scope=detail */
const MSG_MAX = 200;
/** 折叠计数表分组上限：健康机器几十组封顶，异常 query 组合也有界 */
const COUNTS_MAX = 100;

/** 纯函数：spec（单组或数组=多组 OR）→ 写死的 pwsh 命令串。
 *  只有经过 buildSpec 校验的字段才会插值进来。
 *  多组：boot 需要（ID 白名单）OR（WHEA-Logger 提供者），而 FilterHashtable
 *  跨字段只能 AND，故按组下推后流式去重（$seen：logName/recordId —— WHEA
 *  声明的 ID 19 与白名单的 19 重叠，去重是真实需要的）。
 *  多组查询允许共享同一后置过滤（msgLike/providerRe）。 */
export function buildEventQueryCmd(specs: CoreQuery | CoreQuery[]): string {
	const list = Array.isArray(specs) ? specs : [specs];
	if (list.length === 0) throw new Error("至少需要一个查询组");
	const top = list[0].top;
	if (list.some((s) => s.msgLike !== list[0].msgLike || s.providerRe !== list[0].providerRe)) {
		throw new Error("多组查询必须使用相同的 msgLike/providerRe 后置过滤");
	}
	const msgLike = list.find((s) => s.msgLike)?.msgLike;
	const providerRe = list.find((s) => s.providerRe)?.providerRe;

	const lines: string[] = [];
	lines.push("$ErrorActionPreference = 'SilentlyContinue'");
	lines.push(`$top = ${top}`);
	// 各组 FilterHashtable：结构化下推，模型输入只出现在白名单校验后的结构化字段
	lines.push("$groups = @(");
	for (const s of list) {
		const f: string[] = [`LogName = @(${s.logNames.map(psString).join(",")})`];
		if (s.level === "warn") f.push("Level = @(1,2,3)"); // Warning 及更严重
		if (s.level === "error") f.push("Level = @(1,2)"); // Error 及更严重
		if (s.ids) f.push(`Id = @(${s.ids.join(",")})`);
		if (s.providers) f.push(`ProviderName = @(${s.providers.map(psString).join(",")})`);
		f.push(`StartTime = (Get-Date).AddHours(-${s.hours})`);
		lines.push(`  @{ ${f.join("; ")} }`);
	}
	lines.push(")");

	// 后置过滤（设计：模糊匹配 pwsh 侧后置，ID 白名单已下推，后置量小）。
	// provider 正则带 1s MatchTimeout（防灾难性回溯），编译失败结构化上报；
	// 消息子串走 IndexOf OrdinalIgnoreCase（无正则，零回溯风险）。
	if (providerRe) {
		lines.push("$provRe = $null; $reErr = $null");
		lines.push(
			`try { $provRe = [regex]::new(${psString(providerRe)}, [System.Text.RegularExpressions.RegexOptions]::IgnoreCase, [timespan]::FromSeconds(1)) } catch { $reErr = [string]$_.Exception.Message }`,
		);
	}
	if (msgLike) lines.push(`$msgLike = ${psString(msgLike)}`);

	// 逐组流式枚举（组内时间倒序）：计数表只读 ProviderName/Id/TimeCreated；
	// 完整记录每组最多物化 top 条，多组合并后统一排序取全局 top ——
	// 内存不随命中量增长（坏机器一天几千条同理）。
	lines.push(`$total = 0`);
	lines.push(`$counts = @{}`);
	lines.push(`$seen = @{}`);
	lines.push(`$events = [System.Collections.Generic.List[object]]::new()`);
	lines.push(`foreach ($ht in $groups) {`);
	lines.push(`  $kept = 0`);
	lines.push(`  Get-WinEvent -FilterHashtable $ht | ForEach-Object {`);
	lines.push(`    $key = "$($_.LogName)/$($_.RecordId)"`);
	lines.push(`    if ($seen.ContainsKey($key)) { return }`);
	lines.push(`    $seen[$key] = 1`);
	lines.push(`    $prov = "$($_.ProviderName)"`);
	lines.push(`    if ($provRe) { try { if (-not $provRe.IsMatch($prov)) { return } } catch { return } }`);
	lines.push(`    $m = $null`);
	lines.push(`    if ($msgLike) {`);
	lines.push(`      $m = $_.Message`);
	lines.push(`      if (-not $m -or $m.IndexOf($msgLike, [StringComparison]::OrdinalIgnoreCase) -lt 0) { return }`);
	lines.push(`    }`);
	lines.push(`    $total++`);
	lines.push(`    $eid = $_.Id`);
	lines.push(`    $tc = $_.TimeCreated.ToString('yyyy-MM-dd HH:mm')`);
	lines.push(`    $k = "$prov/$eid"`);
	lines.push(`    $c = $counts[$k]`);
	lines.push(
		`    if ($c) { $c.n++; if ($tc -gt $c.last) { $c.last = $tc } } else { $counts[$k] = @{ n = 1; last = $tc; prov = $prov; id = $eid } }`,
	);
	lines.push(`    if ($kept -lt $top) {`);
	lines.push(`      if (-not $m) { $m = $_.Message }`);
	lines.push(
		`      if ($m) { $m = ($m -replace '\\s+',' ').Trim(); if ($m.Length -gt ${MSG_MAX}) { $m = $m.Substring(0,${MSG_MAX}) + '…' } }`,
	);
	lines.push(
		`      $events.Add([pscustomobject]@{ logName = $_.LogName; time = $_.TimeCreated.ToString('yyyy-MM-dd HH:mm:ss'); recordId = $_.RecordId; level = $_.Level; provider = $prov; id = $eid; msg = $m })`,
	);
	lines.push(`      $kept++`);
	lines.push(`    }`);
	lines.push(`  }`);
	lines.push(`}`);
	// 多组合并后统一时间倒序；同秒内按 recordId 降序保持确定性（单组为无操作）
	lines.push(
		"$events = @($events | Sort-Object -Property @{Expression='time';Descending=$true},@{Expression='recordId';Descending=$true} | Select-Object -First $top)",
	);

	// $Error 分类（语言无关判据，见文件头实测结论）：
	// EventLogException = 毒事件渲染失败（该记录被跳过，计数即可）；
	// NoMatchingEventsFound / NoMatchingProvidersFound / LogsAndProvidersDontOverlap
	//   = 查询条件匹配不到东西（零命中，正常；后两者在 noMatch 里给固定码供 notice 提示）；
	// 其余（NoMatchingLogsFound 等）= 结构性错误，逐条透传给 Node 收敛。
	lines.push(`$unreadable = 0`);
	lines.push(`$structural = @()`);
	lines.push(`$noMatch = @()`);
	lines.push(`foreach ($e in $Error) {`);
	lines.push(`  $t = $e.Exception.GetType().Name`);
	lines.push(`  if ($t -eq 'EventLogException') { $unreadable++ }`);
	lines.push(`  else {`);
	lines.push(`    $f = $e.FullyQualifiedErrorId`);
	lines.push(`    if ($f -like 'NoMatchingEventsFound*') { }`);
	lines.push(`    elseif ($f -like 'NoMatchingProvidersFound*') { $noMatch += 'provider-not-found' }`);
	lines.push(`    elseif ($f -like 'LogsAndProvidersDontOverlap*') { $noMatch += 'provider-log-mismatch' }`);
	lines.push(`    else { $structural += [string]$e.Exception.Message }`);
	lines.push(`  }`);
	lines.push(`}`);
	lines.push(`$noMatch = @($noMatch | Sort-Object -Unique)`);
	lines.push(
		"$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
	);

	lines.push(`$countsOut = @($counts.Values | Sort-Object n -Descending | Select-Object -First ${COUNTS_MAX})`);
	lines.push(`ConvertTo-Json @{`);
	lines.push(`  total = $total`);
	lines.push(`  truncated = ($total -gt $top)`);
	lines.push(`  unreadable = $unreadable`);
	lines.push(`  noMatch = $noMatch`);
	lines.push(`  structural = @($structural | Select-Object -First 3)`);
	lines.push(`  reErr = $reErr`);
	lines.push(`  admin = $admin`);
	lines.push(`  countsTruncated = ($counts.Count -gt ${COUNTS_MAX})`);
	lines.push(`  events = $events`);
	lines.push(`  counts = $countsOut`);
	lines.push(`} -Depth 4`);
	return lines.join("\n");
}

/* ------------------------------------------------------------------ */
/* 返回收敛：体积硬规则（top 上限 / 折叠计数表 / 200 字符 / total + notice） */
/* ------------------------------------------------------------------ */

export interface CoreEvent {
	logName: string;
	time: string; // 'yyyy-MM-dd HH:mm:ss'（本地时间）
	recordId: number; // 记录号，scope=detail 直取原文用
	level: number; // 1=Critical 2=Error 3=Warning 4=Information
	levelName: string;
	provider: string;
	id: number;
	msg: string | null; // ≤200 字符单行简述；无消息文本为 null
	atypical?: boolean; // crash scope 专用：provider 非典型崩溃来源（ID 可能被第三方复用），请人工判读
}

export interface CountRow {
	key: string; // 'provider/id'
	provider: string;
	id: number;
	n: number; // 时间窗内出现次数
	last: string; // 最近一次 'yyyy-MM-dd HH:mm'
}

export interface CoreResultData {
	logs: string[];
	hours: number;
	top: number;
	level?: LevelTier; // 最低级别档回显（scope 未传时不字段；多组级别不一致时不回显）
	ids?: number[]; // ID 白名单并集回显（多组白名单取全组并集）
	providers?: string[]; // 提供程序并集回显（多组白名单取全组并集）
	total: number; // 时间窗内命中总数（含未显示的更早记录）
	truncated: boolean; // total > top，事件列表被截断
	unreadable: number; // 消息资源损坏被跳过的记录数
	noMatch: string[]; // 条件级零命中提示码：provider-not-found / provider-log-mismatch
	admin: boolean; // 查询进程是否管理员（security 降级判定用）
	events: CoreEvent[]; // 最新 top 条，时间倒序
	firstTime?: string; // 事件样本最早时间（events 末条），空列表时无此字段；日志被刷屏滚没时样本跨度会远小于 hours 窗口
	lastTime?: string; // 事件样本最新时间（events 首条）
	counts: CountRow[]; // 来源/ID 折叠计数表，n 降序
	countsTruncated: boolean;
}

export interface CoreResult {
	data?: CoreResultData;
	notice?: string;
	error?: string;
	admin?: boolean; // 结构性错误时也回带，security 降级判定用
}

/** 数字级别 → 固定英文（不随系统语言本地化，模型可稳定解读） */
export function levelName(n: unknown): string {
	switch (n) {
		case 1:
			return "Critical";
		case 2:
			return "Error";
		case 3:
			return "Warning";
		case 4:
			return "Information";
		default:
			return `Level${String(n)}`;
	}
}

/** 简述收敛：折叠空白为单行、截 200 字符（超出补 …）。与 pwsh 侧规则一致且幂等。 */
export function truncateBrief(msg: unknown, max = MSG_MAX): string | null {
	if (typeof msg !== "string") return null;
	let m = msg.replace(/\s+/g, " ").trim();
	if (m.length > max) m = `${m.slice(0, max)}…`;
	return m.length > 0 ? m : null;
}

/** 核心查询：spec 归一化（单组或数组=多组 OR）→ 下推执行 → 收敛为 { data, notice } 或 { error } */
export async function queryEvents(
	raw: Parameters<typeof buildSpec>[0] | Parameters<typeof buildSpec>[0][],
	signal?: AbortSignal,
): Promise<CoreResult> {
	signal?.throwIfAborted();
	const raws = Array.isArray(raw) ? raw : [raw];
	let specs: CoreQuery[];
	try {
		specs = raws.map((r) => buildSpec(r));
	} catch (e) {
		signal?.throwIfAborted();
		return { error: errorMessage(e).slice(0, 300) };
	}

	const r = asRecord(await runPwsh(buildEventQueryCmd(specs), { signal }));
	if (r && typeof r.error === "string") return { error: r.error };
	if (!r || typeof r.total !== "number" || !Array.isArray(r.events) || !Array.isArray(r.counts)) {
		return { error: "事件日志查询返回结构异常" };
	}
	if (r.reErr) return { error: `provider 正则无法编译: ${truncateBrief(r.reErr, 160)}` };

	// 结构性错误（通道不存在等）：诚实报错，不吞。固定通道（System/Application）
	// 正常不可达此路径；query 自定义通道的预校验见 queryDetail/runScope 的错误收敛。
	const structural: string[] = Array.isArray(r.structural)
		? r.structural.filter((s: unknown) => typeof s === "string" && s.trim())
		: [];
	if (structural.length > 0) {
		return {
			error: `事件日志查询失败（${structural.length} 处）: ${truncateBrief(structural[0], 200)}`,
			admin: !!r.admin,
		};
	}

	const events: CoreEvent[] = asRecords(r.events).map((e) => {
		const lv = Number(e.level ?? 0);
		return {
			logName: String(e.logName ?? ""),
			time: String(e.time ?? ""),
			recordId: Number(e.recordId ?? 0),
			level: lv,
			levelName: levelName(lv),
			provider: String(e.provider ?? ""),
			id: Number(e.id ?? 0),
			msg: truncateBrief(e.msg), // Node 侧兜底二次收敛（与 pwsh 侧规则一致，幂等）
		};
	});

	const counts: CountRow[] = asRecords(r.counts).map((c) => ({
		key: `${c.prov}/${c.id}`,
		provider: String(c.prov ?? ""),
		id: Number(c.id ?? 0),
		n: Number(c.n ?? 0),
		last: String(c.last ?? ""),
	}));

	// 头部回显口径：多组白名单（boot/crash）取全组并集，避免只回显第一组造成失真；
	// 各组级别一致才回显 level，否则置空（级别口径由各 scope 的白名单描述说明）
	const idUnion = [...new Set(specs.flatMap((s) => s.ids ?? []))];
	const provUnion = [...new Set(specs.flatMap((s) => s.providers ?? []))];
	const levelSet = new Set(specs.map((s) => s.level ?? ""));
	const data: CoreResultData = {
		logs: [...new Set(specs.flatMap((s) => s.logNames))],
		hours: specs[0].hours,
		top: specs[0].top,
		level: levelSet.size === 1 ? specs[0].level : undefined,
		ids: idUnion.length > 0 ? idUnion : undefined,
		providers: provUnion.length > 0 ? provUnion : undefined,
		total: r.total,
		truncated: !!r.truncated,
		unreadable: Number(r.unreadable ?? 0),
		noMatch: Array.isArray(r.noMatch) ? r.noMatch.map(String) : [],
		admin: !!r.admin,
		events,
		counts,
		countsTruncated: !!r.countsTruncated,
	};

	// notice：截断说明（设计硬规则：data.total + notice 说明截断）+ 字段导读
	const parts: string[] = [];
	parts.push(
		`近 ${data.hours} 小时 ${data.logs.join(" + ")}${data.level ? `（level=${data.level} 及更严重）` : ""} 命中 ${data.total} 条`,
	);
	parts.push(
		data.truncated
			? `已按 top=${data.top} 截断，仅显示最新 ${events.length} 条（时间倒序），更早的记录不在列表中`
			: `显示全部 ${events.length} 条（时间倒序）`,
	);
	if (data.unreadable > 0)
		parts.push(`另有 ${data.unreadable} 条记录因消息资源损坏无法读取，已跳过（不影响其余统计）`);
	if (data.noMatch.includes("provider-not-found")) parts.push("查询条件中的提供程序在本机不存在");
	if (data.noMatch.includes("provider-log-mismatch")) parts.push("查询条件中的提供程序不写往所查通道");
	if (data.countsTruncated) parts.push(`重复来源过多，计数表仅列前 ${COUNTS_MAX} 组`);
	if (events.length > 0) {
		data.firstTime = String(events[events.length - 1].time ?? "");
		data.lastTime = String(events[0].time ?? "");
	}
	if (events.length > 0) parts.push(`事件样本覆盖 ${data.firstTime} ~ ${data.lastTime}`);
	parts.push(
		`counts=来源/ID 折叠计数表（n=出现次数，last=最近一次）；msg=简述（截 ${MSG_MAX} 字符），原文用 scope=detail 按 recordId 取`,
	);
	return { data, notice: `${parts.join("。")}。` };
}

/* ------------------------------------------------------------------ */
/* scope 白名单（来源与核定见 doc/design/tool/event-design.md 及文件头实测结论） */
/* ------------------------------------------------------------------ */

/** boot：官方重启排查清单（all 组合全部） */
export const BOOT_IDS_ALL = [12, 13, 6005, 6006, 6009, 41, 6008, 1001, 1074, 19, 7045];
export const BOOT_IDS_UNEXPECTED = [41, 6008]; // 意外重启 / 意外关机
export const BOOT_IDS_BLUESCREEN = [1001]; // System 通道 = BugCheck（Application 通道的同 ID 是 WER，归 crash）
export const WHEA_PROVIDER = "Microsoft-Windows-WHEA-Logger"; // 精确名实测确认；声明含 ID 19，与白名单重叠 → 多组去重

/** crash：官方崩溃文档 1000/1001 + 启动失败补充 1026/33·35 */
export const CRASH_IDS = [1000, 1001, 1002, 1026, 33, 35];
/** Application Error / .NET Runtime 限 Error；WER 与 Hang 保留原始级别。 */
export const CRASH_IDS_ERROR = [1000, 1026];
/** 33/35（SideBySide）级别未逐条核实，保持不限级别，避免漏报 */
export const CRASH_IDS_ANY = [33, 35];
/** crash 的典型崩溃来源 provider（软标注用）：命中但 provider 不在名单内的标 atypical */
export const CRASH_TYPICAL_PROVIDERS = [
	"Application Error", // 1000
	"Windows Error Reporting", // 1001
	".NET Runtime", // 1026
	"Application Hang", // 1002
	"SideBySide", // 33/35
];

/** service：对 SCM 提供者全表交叉核对（ListProvider 解码 0xC0000000+N，
 *  全部 Error 级；7025 本机表未声明，按设计保留，下推多一个永不命中的 ID 无害）；
 *  明确不收：一次性配置类与 7035/7036/7039/7040/7042/7044 信息级 */
export const SERVICE_IDS = [
	7000, 7001, 7002, 7003, 7009, 7011, 7013, 7022, 7023, 7024, 7025, 7026, 7031, 7032, 7034, 7038, 7041, 7043,
];

/** disk：坏块/控制器/分页 7·11·51；超时重试 129·153；Ntfs 损坏/写失败 55·98·50·140；掉盘 157 */
export const DISK_IDS = [7, 11, 51, 55, 98, 50, 129, 140, 153, 157];

/** security：本地/工作组场景；域环境 Kerberos 失败（4771）不覆盖（设计定界） */
export const SECURITY_IDS: Record<string, number[]> = {
	all: [4624, 4625, 4740],
	logonFail: [4625],
	lockout: [4740],
};

export const SCOPES = ["recent", "boot", "crash", "service", "disk", "security", "query", "detail"] as const;
export type ScopeName = (typeof SCOPES)[number];

/* ------------------------------------------------------------------ */
/* 管理员预检（security 降级用；进程内缓存——运行期间身份不变）              */
/* ------------------------------------------------------------------ */

let adminCache: boolean | null = null;

export async function isAdminPwsh(signal?: AbortSignal): Promise<boolean> {
	signal?.throwIfAborted();
	if (adminCache !== null) return adminCache;
	const r = await runPwsh(
		"ConvertTo-Json ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
		{ timeoutMs: 10000, signal },
	);
	if (typeof r !== "boolean") throw new Error(`管理员身份探测失败: ${asRecord(r).error ?? "返回结构异常"}`);
	adminCache = r;
	return adminCache;
}

/* ------------------------------------------------------------------ */
/* scope=detail：单条详情与原文（recordId 直取 / id 取最近一条）             */
/* ------------------------------------------------------------------ */

/** 原文上限（字符）：单条输出有界；超出截断并标记 */
const MSG_FULL_MAX = 20000;

export function buildDetailCmd(spec: { logName: string; recordId?: number; id?: number }): string {
	const lines: string[] = [];
	lines.push("$ErrorActionPreference = 'SilentlyContinue'");
	lines.push(
		"$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
	);
	if (spec.recordId !== undefined) {
		// XPath 元素名是 EventRecordID（PS 属性叫 RecordId）；模板写死，仅插入整数
		// 校验后的 recordId —— 与 FilterHashtable 插值同等级的安全模型
		lines.push(
			`$rec = Get-WinEvent -LogName ${psString(spec.logName)} -FilterXPath '*[System[(EventRecordID=${spec.recordId})]]' -MaxEvents 1`,
		);
	} else {
		lines.push(
			`$rec = Get-WinEvent -FilterHashtable @{ LogName = ${psString(spec.logName)}; Id = ${spec.id} } -MaxEvents 1`,
		);
	}
	lines.push("$unreadable = 0");
	lines.push("$structural = @()");
	lines.push("foreach ($e in $Error) {");
	lines.push("  $t = $e.Exception.GetType().Name");
	lines.push("  if ($t -eq 'EventLogException') { $unreadable++ }");
	lines.push("  elseif ($e.FullyQualifiedErrorId -like 'NoMatchingEventsFound*') { }");
	lines.push("  else { $structural += [string]$e.Exception.Message }");
	lines.push("}");
	lines.push("if ($rec) {");
	lines.push("  $m = [string]$rec.Message");
	lines.push(`  $cut = $false`);
	lines.push(
		`  if ($m.Length -gt ${MSG_FULL_MAX}) { $m = $m.Substring(0,${MSG_FULL_MAX}) + '…(已截断)'; $cut = $true }`,
	);
	lines.push(
		"  ConvertTo-Json @{ found = $true; admin = $admin; cut = $cut; structural = @(); ev = @{ logName = $rec.LogName; recordId = $rec.RecordId; time = $rec.TimeCreated.ToString('yyyy-MM-dd HH:mm:ss'); level = $rec.Level; provider = $rec.ProviderName; id = $rec.Id; machine = $rec.MachineName; msg = $m } } -Depth 3",
	);
	lines.push("} else {");
	lines.push(
		"  ConvertTo-Json @{ found = $false; admin = $admin; structural = @($structural | Select-Object -First 3); poison = $unreadable } -Depth 3",
	);
	lines.push("}");
	return lines.join("\n");
}

export interface DetailCoreResult {
	data?: Record<string, unknown>;
	notice?: string;
	error?: string;
	admin?: boolean;
}

export async function queryDetail(
	raw: {
		logName?: unknown;
		recordId?: unknown;
		id?: unknown;
	},
	signal?: AbortSignal,
): Promise<DetailCoreResult> {
	signal?.throwIfAborted();
	let logName: string;
	let recordId: number | undefined;
	let id: number | undefined;
	try {
		if (typeof raw.logName !== "string" || !RE_LOGNAME.test(raw.logName)) {
			throw new Error(`detail 需要合法 logName（通道名），收到: ${JSON.stringify(raw.logName ?? null)}`);
		}
		logName = raw.logName;
		const hasR = raw.recordId !== undefined && raw.recordId !== null;
		const hasI = raw.id !== undefined && raw.id !== null;
		if (hasR && hasI) throw new Error("recordId 与 id 只能二选一");
		if (!hasR && !hasI) throw new Error("detail 需要 recordId 或 id 之一");
		if (hasR) {
			const n = asInt(raw.recordId);
			if (n === null || n < 1 || n > 9007199254740991) {
				throw new Error(`非法 recordId: ${JSON.stringify(raw.recordId)}（只接受正整数）`);
			}
			recordId = n;
		} else {
			const n = asInt(raw.id);
			if (n === null || n < 0 || n > 65535) {
				throw new Error(`非法 id: ${JSON.stringify(raw.id)}（只接受 0-65535 的整数）`);
			}
			id = n;
		}
	} catch (e) {
		signal?.throwIfAborted();
		return { error: errorMessage(e).slice(0, 300) };
	}

	const r = asRecord(await runPwsh(buildDetailCmd({ logName, recordId, id }), { signal }));
	if (r && typeof r.error === "string") return { error: r.error };
	if (!r || typeof r.found !== "boolean") return { error: "事件详情返回结构异常" };

	const structural: string[] = Array.isArray(r.structural)
		? r.structural.filter((s: unknown) => typeof s === "string" && s.trim())
		: [];
	if (r.found === true) {
		const ev = asRecord(r.ev ?? {});
		const lv = Number(ev.level ?? 0);
		const msg = typeof ev.msg === "string" ? ev.msg : null;
		const cut = !!r.cut;
		return {
			data: {
				found: true,
				logName: String(ev.logName ?? logName),
				recordId: Number(ev.recordId ?? 0),
				time: String(ev.time ?? ""),
				level: lv,
				levelName: levelName(lv),
				provider: String(ev.provider ?? ""),
				id: Number(ev.id ?? 0),
				machine: String(ev.machine ?? ""),
				msg, // 完整原文（保留换行），仅超长时截断并标记
			},
			notice: `msg=完整原文（${msg ? msg.length : 0} 字符${cut ? "，已截断" : ""}，保留换行），简述版见各 scope 列表；时间 ${String(ev.time ?? "")}。`,
		};
	}
	if (structural.length > 0) {
		return { error: `事件日志查询失败: ${truncateBrief(structural[0], 200)}`, admin: !!r.admin };
	}
	if (Number(r.poison ?? 0) > 0) {
		return { error: "记录无法读取（消息资源损坏）", admin: !!r.admin };
	}
	return {
		data: { found: false },
		notice: `未找到该记录（logName=${logName}${recordId !== undefined ? ` recordId=${recordId}` : ` id=${id}`}）；记录可能已被滚动清除或不在此通道。`,
	};
}

/* ------------------------------------------------------------------ */
/* scope 路由：模型参数 → CoreQuery → 收敛 payload（eventlog.ts 只做注册）   */
/* ------------------------------------------------------------------ */

export interface ScopeParams {
	scope?: unknown;
	kind?: unknown;
	type?: unknown;
	level?: unknown;
	app?: unknown;
	name?: unknown;
	provider?: unknown;
	msg?: unknown;
	ids?: unknown;
	logName?: unknown;
	recordId?: unknown;
	id?: unknown;
	hours?: unknown;
	top?: unknown;
}

function optStr(v: unknown, field: string): string | undefined {
	if (v === undefined || v === null) return undefined;
	if (typeof v !== "string") throw new Error(`${field} 必须是字符串，收到: ${JSON.stringify(v)}`); // 静默丢弃会让模型误以为过滤生效
	const s = v.trim();
	return s || undefined;
}

function payload(r: CoreResult, prefix?: string): Record<string, unknown> {
	if (r.error) {
		return r.admin !== undefined ? { error: r.error, admin: r.admin } : { error: r.error };
	}
	return { ...(r.data as object), notice: (prefix ?? "") + (r.notice ?? "") };
}

export async function runScope(params: ScopeParams, signal?: AbortSignal): Promise<Record<string, unknown>> {
	signal?.throwIfAborted();
	const scope = typeof params.scope === "string" ? params.scope : "recent"; // 无 scope 兜底 recent
	const { hours, top } = params;
	try {
		return await routeScope(scope, params, hours, top, signal);
	} catch (e) {
		signal?.throwIfAborted();
		return { error: errorMessage(e).slice(0, 300) };
	}
}

async function routeScope(
	scope: string,
	params: ScopeParams,
	hours: unknown,
	top: unknown,
	signal?: AbortSignal,
): Promise<Record<string, unknown>> {
	signal?.throwIfAborted();
	switch (scope) {
		case "recent": {
			const r = await queryEvents(
				{
					logNames: ["System", "Application"],
					level: params.level ?? "warn",
					hours,
					top,
				},
				signal,
			);
			return payload(r);
		}
		case "boot": {
			const kind = params.kind ?? "all";
			if (kind !== "all" && kind !== "unexpected" && kind !== "bluescreen") {
				return { error: `非法 kind: ${JSON.stringify(kind)}（只接受 all / unexpected / bluescreen）` };
			}
			const groups: Parameters<typeof queryEvents>[0] =
				kind === "unexpected"
					? [{ logNames: ["System"], ids: BOOT_IDS_UNEXPECTED, hours, top }]
					: kind === "bluescreen"
						? [
								{ logNames: ["System"], ids: BOOT_IDS_BLUESCREEN, hours, top },
								{ logNames: ["System"], providers: [WHEA_PROVIDER], hours, top },
							]
						: [
								{ logNames: ["System"], ids: BOOT_IDS_ALL, hours, top },
								{ logNames: ["System"], providers: [WHEA_PROVIDER], hours, top },
							];
			const desc =
				kind === "unexpected"
					? "意外重启 41（Kernel-Power）/ 6008（意外关机）"
					: kind === "bluescreen"
						? "蓝屏详情 1001（BugCheck）+ WHEA-Logger 硬件错误"
						: "启停标记 12/13/6005/6006/6009；意外重启 41/6008；蓝屏 1001；重启原因 1074/19/7045；WHEA-Logger 硬件错误";
			const r = await queryEvents(groups, signal);
			const out = payload(r, `boot(kind=${kind})：白名单=${desc}。`);
			if (!out.error) out.kind = kind;
			return out;
		}
		case "crash": {
			const app = optStr(params.app, "app");
			// 提供程序与 ID 配对，避免第三方复用 ID；WER / Hang 保留原始级别。
			const groups: Parameters<typeof queryEvents>[0] = [
				{
					logNames: ["Application"],
					ids: [1000],
					providers: ["Application Error"],
					level: "error",
					msgLike: app,
					hours,
					top,
				},
				{ logNames: ["Application"], ids: [1002], providers: ["Application Hang"], msgLike: app, hours, top },
				{
					logNames: ["Application"],
					ids: [1026],
					providers: [".NET Runtime"],
					level: "error",
					msgLike: app,
					hours,
					top,
				},
				{
					logNames: ["Application"],
					ids: [1001],
					providers: ["Windows Error Reporting"],
					msgLike: app,
					hours,
					top,
				},
				{ logNames: ["Application"], ids: CRASH_IDS_ANY, providers: ["SideBySide"], msgLike: app, hours, top },
			];
			const r = await queryEvents(groups, signal);
			const out = payload(
				r,
				`crash：白名单=崩溃 1000 / WER 1001 / 无响应 1002 / .NET Runtime 1026（按提供程序限定；WER/无响应保留原始级别）/ SideBySide 33·35（不限级别）。${app ? `app 过滤=「${app}」（消息子串，不区分大小写）。` : ""}atypical=true=provider 非典型崩溃来源，请人工判读。`,
			);
			if (!out.error) {
				// 软标注：Error 级命中里 provider 不属于典型崩溃来源的，标出来请人工判读
				const typical = new Set(CRASH_TYPICAL_PROVIDERS.map((p) => p.toLowerCase()));
				for (const ev of ((out.events as CoreEvent[]) ?? []) as (CoreEvent & { atypical?: boolean })[]) {
					signal?.throwIfAborted();
					if (!typical.has(ev.provider.toLowerCase())) ev.atypical = true;
				}
			}
			if (!out.error && app) out.app = app;
			return out;
		}
		case "service": {
			const name = optStr(params.name, "name");
			const r = await queryEvents({ logNames: ["System"], ids: SERVICE_IDS, msgLike: name, hours, top }, signal);
			const out = payload(
				r,
				`service：白名单=SCM 启动失败 7000-7003 / 账户密码 7013·7038·7041 / 超时挂起 7009·7011·7022 / 异常终止 7023·7024·7031·7032·7034 / 未正常关闭 7043 / 驱动加载 7025·7026。${name ? `name 过滤=「${name}」（消息子串，不区分大小写）。` : ""}`,
			);
			if (!out.error && name) out.name = name;
			return out;
		}
		case "disk": {
			const r = await queryEvents({ logNames: ["System"], ids: DISK_IDS, hours, top }, signal);
			return payload(
				r,
				"disk：白名单=坏块/控制器/分页 7·11·51 / 超时重试 129·153 / Ntfs 损坏/写失败 55·98·50·140 / 掉盘 157。",
			);
		}
		case "security": {
			const type = params.type ?? "all";
			if (type !== "all" && type !== "logonFail" && type !== "lockout") {
				return { error: `非法 type: ${JSON.stringify(type)}（只接受 all / logonFail / lockout）` };
			}
			// 非管理员时 FilterHashtable 查 Security 会静默返回 0 条（实测），
			// 必须显式预检降级，否则「没权限」会伪装成「没有登录事件」
			const admin = await isAdminPwsh(signal);
			if (!admin) {
				return {
					admin: false,
					degraded: true,
					type,
					notice:
						"security 需要管理员权限：当前以非管理员运行，Security 日志不可读，未执行查询。以管理员身份重启 pi 后可用（type=all / logonFail / lockout）。",
				};
			}
			const r = await queryEvents({ logNames: ["Security"], ids: SECURITY_IDS[type], hours, top }, signal);
			const out = payload(
				r,
				`security(type=${type})：白名单=成功登录 4624 / 登录失败 4625 / 账户锁定 4740（域环境 Kerberos 失败不在覆盖范围）。`,
			);
			if (!out.error) {
				out.type = type;
				out.admin = true;
			}
			return out;
		}
		case "query": {
			const logName = optStr(params.logName, "logName");
			const provider = optStr(params.provider, "provider");
			const msg = optStr(params.msg, "msg");
			const r = await queryEvents(
				{
					logNames: logName ? [logName] : ["System", "Application"],
					level: params.level,
					ids: params.ids,
					providerRe: provider,
					msgLike: msg,
					hours,
					top,
				},
				signal,
			);
			const bits: string[] = [logName ? `通道=${logName}` : "通道=System+Application"];
			if (params.level) bits.push(`level=${String(params.level)}`);
			if (params.ids !== undefined) bits.push("ids 下推");
			if (provider) bits.push(`provider≈/${provider}/`);
			if (msg) bits.push(`msg 含「${msg}」`);
			return payload(r, `query：自定义查询（${bits.join("；")}）。`);
		}
		case "detail": {
			const r = await queryDetail({ logName: params.logName, recordId: params.recordId, id: params.id }, signal);
			if (r.error) return { error: r.error };
			if (r.data?.found) {
				const { found, ...ev } = r.data as { found: boolean } & Record<string, unknown>;
				return { ...ev, found, notice: r.notice };
			}
			return { found: false, notice: r.notice };
		}
		default:
			return { error: `未知 scope: ${scope}（当前支持 ${SCOPES.join(" / ")}）` };
	}
}

/** pi 加载器会自动加载 extensions\ 下所有 .ts；共享模块用空 factory 保持安静（wz-index 同款）。 */
