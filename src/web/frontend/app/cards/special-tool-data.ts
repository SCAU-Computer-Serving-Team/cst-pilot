import type { Message } from "../data/web-state";
import { type SpecialToolCall, specialToolKind } from "./tool-kind.ts";
import { type ToolState, toolState } from "./tool-state.ts";

export { type SpecialToolCall, type SpecialToolKind, specialToolKind } from "./tool-kind.ts";
export type SpecialStatus = ToolState | "cancelled" | "unknown";
export type SpecialField = { label: string; value: string };
export type SpecialVolume = {
	id: string;
	drive: string;
	conversion: string;
	conversionCode: number | null;
	lock: string;
	percentage: number | null;
	operation: string;
	reason: string;
	errors: string[];
};
type Base = {
	status: SpecialStatus;
	title: string;
	summary: string;
	raw: string;
	fields: SpecialField[];
	errors: string[];
};
export type SpecialToolView =
	| (Base & {
			kind: "sys-overview";
			machine: string;
			cpu: number | null;
			cores: number | null;
			memory: number | null;
			capacity: string;
			uptime: string;
	  })
	| (Base & {
			kind: "runbook";
			commands: { shell: string; admin: boolean; summary: string; command: string }[];
			level: string;
			count: number | null;
			version: number | null;
			planned: boolean;
			delivery?: {
				name: string;
				file: string;
				level: string;
				count: number | null;
			};
	  })
	| (Base & { kind: "web-search"; queries: string[]; successful: number | null; unstructured: boolean })
	| (Base & { kind: "bitlocker"; scope: "unlock" | "decrypt"; volumes: SpecialVolume[] });

const record = (v: unknown): Record<string, unknown> | undefined =>
	v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : undefined;
const string = (v: unknown, fallback = "") => (typeof v === "string" ? v : fallback);
const number = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : null);
const list = (v: unknown) => (Array.isArray(v) ? v : []);
const objects = (v: unknown) =>
	list(v)
		.map(record)
		.filter((x): x is Record<string, unknown> => !!x);
const gb = (v: unknown) => (number(v) === null ? "未知" : `${(Number(v) / 1024).toFixed(1).replace(/\.0$/, "")}`);
const rawOf = (result?: Message) =>
	!result
		? ""
		: typeof result.content === "string"
			? result.content
			: result.content
					.filter((p) => p.type === "text")
					.map((p) => p.text)
					.join("\n");
const errorText = (v: unknown): string => {
	const e = record(v);
	if (!e) return v == null || v === "" ? "" : String(v);
	const code = number(e.code);
	const text = [
		string(e.source),
		code === null ? "" : `0x${code.toString(16).toUpperCase().padStart(8, "0")}`,
		string(e.message),
	]
		.filter(Boolean)
		.join(" · ");
	return text || JSON.stringify(e);
};
const errorsOf = (...values: unknown[]): string[] =>
	values.flatMap((v) => (Array.isArray(v) ? v.map(errorText) : [errorText(v)])).filter(Boolean);
const field = (label: string, value: unknown): SpecialField => ({
	label,
	value: value == null ? "未知" : String(value),
});
export const conversionLabel = (v: number | null) =>
	v === null
		? "状态未确认"
		: ((
				{ 0: "完全解密", 1: "已加密", 2: "加密中", 3: "解密中", 4: "加密暂停", 5: "解密暂停" } as Record<
					number,
					string
				>
			)[v] ?? `未知状态 (${v})`);
export const lockLabel = (v: number | null) => (v === 0 ? "已解锁" : v === 1 ? "已锁定" : "锁定状态未确认");

export function mapSpecialTool(call: SpecialToolCall, result?: Message, live = false): SpecialToolView | undefined {
	const kind = specialToolKind(call);
	if (!kind) return undefined;
	const details = record(result?.details);
	const raw = rawOf(result);
	const initial = toolState(result, "success", live, call.execution?.startedAt !== undefined);
	const commonErrors = errorsOf(details?.error, result?.isError ? raw : undefined);
	const base: Base = {
		status: initial,
		title: "",
		summary:
			initial === "pending"
				? "等待执行"
				: initial === "running"
					? "执行中"
					: initial === "interrupted"
						? "未完成或已中断"
						: "已完成",
		raw,
		fields: [],
		errors: commonErrors,
	};
	if (kind === "sys-overview") {
		const source = record(details?.overview);
		const mem = record(source?.mem),
			machine = record(source?.machine),
			uptime = record(source?.uptime);
		const errors = [...commonErrors, ...errorsOf(source?.error, source?.collectionErrors, source?.counterErrors)];
		const status = !result
			? initial
			: result.isError || source?.error != null
				? "error"
				: !source ||
						source.degraded ||
						errors.length ||
						number(source.cpuTotalPct) === null ||
						number(mem?.usedPct) === null
					? "degraded"
					: "success";
		const fields = [
			field("机型", [string(machine?.vendor), string(machine?.model)].filter(Boolean).join(" · ") || "未知"),
			field("处理器", machine?.cpu),
			field("逻辑核心", source?.logicalCores),
			field(
				"内核池",
				record(source?.pool)?.nonpagedMB == null
					? "未知"
					: `${record(source?.pool)?.nonpagedMB} MB 非分页 / ${record(source?.pool)?.pagedMB ?? "未知"} MB 分页`,
			),
			...objects(source?.pagefile).map((p) =>
				field(string(p.name, "页面文件"), `${gb(p.usedMB)} / ${gb(p.allocMB)} GB`),
			),
			field("开机时间", uptime?.bootTime),
		];
		return {
			...base,
			kind,
			status,
			title: "整机概览",
			summary: !result
				? base.summary === "执行中"
					? "正在采集"
					: base.summary
				: status === "error"
					? "采集失败"
					: status === "degraded"
						? "部分数据缺失"
						: live
							? "刚刚采集"
							: "已采集",
			machine: [
				string(machine?.vendor),
				string(machine?.cpu),
				mem?.totalMB == null ? "" : `${gb(mem.totalMB)} GB 内存`,
			]
				.filter(Boolean)
				.join(" · "),
			cpu: number(source?.cpuTotalPct),
			cores: number(source?.logicalCores),
			memory: number(mem?.usedPct),
			capacity: !mem ? "容量未知" : `${gb(mem.usedMB)} / ${gb(mem.totalMB)} GB`,
			uptime: string(uptime?.text),
			fields: status === "error" ? [] : fields,
			errors,
		};
	}
	if (kind === "runbook") {
		let source = record(details?.runbook);
		if (!source && result && !result.isError) {
			try {
				source = record(record(JSON.parse(raw))?.runbook);
			} catch {
				/* 原文不完整时使用调用预览。 */
			}
		}
		const errors = [...commonErrors, ...errorsOf(source?.error)];
		const status = !result ? initial : result.isError || errors.length ? "error" : !source ? "degraded" : "success";
		const delivery =
			result && status === "success" && typeof source?.file === "string"
				? {
						file: source.file,
						name: string(source.name, "命令清单.txt"),
						level: string(source.levelLabel, "风险档未知"),
						count: number(source.items),
					}
				: undefined;
		const resultCommands = objects(source?.commands);
		const callCommands = objects(call.arguments.items);
		const commands = (Array.isArray(source?.commands) ? resultCommands : callCommands).map((i) => ({
			shell: string(i.shell, "环境未知"),
			admin: i.admin === true,
			summary: string(i.summary),
			command: string(i.command),
		}));
		return {
			...base,
			kind,
			status,
			title: string(source?.title, string(call.arguments.title, "命令清单")),
			count: number(source?.items) ?? (commands.length || null),
			level: string(source?.levelLabel, "风险档未知"),
			version: number(source?.version),
			planned: source?.action === "planned",
			summary: !result
				? base.summary
				: status === "error"
					? "生成失败"
					: source?.action === "planned"
						? "方案已保存"
						: delivery
							? "已生成"
							: "交付结果缺失",
			commands,
			delivery,
			fields: delivery ? [field("文件路径", delivery.file)] : [],
			errors,
		};
	}
	if (kind === "web-search") {
		const values = call.arguments.queries ?? call.arguments.query;
		const queries = (Array.isArray(values) ? values : [values]).filter((q): q is string => typeof q === "string");
		const successful = number(details?.successfulQueries),
			count = number(details?.queryCount) ?? queries.length;
		const partial = successful !== null && successful < count,
			failed = successful === 0 && count > 0;
		const errors = [...commonErrors, ...objects(details?.queries).flatMap((q) => errorsOf(q.error))];
		const status = !result
			? initial
			: result.isError || details?.error != null || failed
				? "error"
				: partial || !details
					? "degraded"
					: "success";
		return {
			...base,
			kind,
			status,
			title: "联网检索",
			summary: !result
				? initial === "running"
					? "正在联网检索…"
					: initial === "pending"
						? "等待联网检索"
						: "联网检索未完成"
				: status === "error"
					? "联网检索失败"
					: status === "degraded"
						? details
							? "部分检索完成"
							: "检索状态未确认"
						: "已联网检索",
			queries,
			successful,
			unstructured: !details,
			errors: errors.length
				? errors
				: failed
					? ["全部查询失败"]
					: partial
						? [`${count - (successful ?? 0)} 项查询未完成`]
						: [],
		};
	}
	const source = record(details?.bitlocker),
		scope = call.arguments.scope === "unlock" ? "unlock" : "decrypt";
	const driveValue = string(call.arguments.drive);
	const drive = /^[a-z]:?$/i.test(driveValue) ? `${driveValue[0]!.toUpperCase()}:` : "目标待确定";
	const volumes = objects(source?.volumes).map((v): SpecialVolume => {
		const op = record(v.operation);
		return {
			id: string(v.volumeId),
			drive: string(v.drive, "无盘符"),
			conversion: conversionLabel(number(v.conversionStatus)),
			conversionCode: number(v.conversionStatus),
			lock: lockLabel(number(v.lockStatus)),
			percentage: number(v.encryptionPercentage),
			operation: string(op?.status),
			reason: string(op?.reason),
			errors: errorsOf(
				op?.error,
				v.collectionErrors,
				op?.returnCode ? { source: "方法返回码", code: op.returnCode } : undefined,
			),
		};
	});
	const errors = [...commonErrors, ...errorsOf(source?.collectionErrors), ...volumes.flatMap((v) => v.errors)];
	const attempted = volumes.filter((v) => v.operation !== "skipped");
	let status: SpecialStatus = initial,
		summary =
			initial === "running"
				? ((
						{
							"正在查询 BitLocker 状态": "检查卷",
							等待确认解密目标: "等待确认",
							等待本地解锁输入或确认: "等待输入",
							正在解锁并复查状态: "解锁中",
							正在提交解密并复查状态: "提交解密中",
						} as Record<string, string>
					)[call.execution?.stage ?? ""] ?? base.summary)
				: base.summary;
	if (result) {
		if (result.isError) {
			status = "error";
			summary = "执行失败";
		} else if (volumes.some((v) => v.operation === "unknown")) {
			status = "unknown";
			summary = "结果未确认";
		} else if (attempted.some((v) => v.operation === "cancelled")) {
			const allCancelled = attempted.every((v) => v.operation === "cancelled");
			const missing = !!source?.degraded || errors.length > 0;
			status = allCancelled && !missing ? "cancelled" : "degraded";
			summary = allCancelled ? (missing ? "已取消，部分状态缺失" : "已取消") : "部分处理已取消";
		} else if (attempted.length && attempted.every((v) => v.operation === "rejected")) {
			status = "error";
			summary = scope === "unlock" ? "解锁失败" : "请求被拒绝";
		} else if (
			!source ||
			source.degraded ||
			errors.length ||
			volumes.some(
				(v) =>
					v.operation === "rejected" ||
					v.lock === "锁定状态未确认" ||
					!["accepted", "skipped", "cancelled", "unknown", "rejected"].includes(v.operation),
			)
		) {
			status = "degraded";
			summary = !source
				? "结果缺少状态信息"
				: volumes.some((v) => v.operation === "rejected")
					? "部分请求失败"
					: volumes.some((v) => !["accepted", "skipped"].includes(v.operation))
						? "请求结果缺失"
						: scope === "unlock" && volumes.every((v) => v.lock === "已解锁")
							? "已解锁，部分状态缺失"
							: scope === "decrypt" && volumes.every((v) => v.conversionCode === 0)
								? "已完全解密，部分状态缺失"
								: scope === "unlock" && volumes.some((v) => v.lock === "锁定状态未确认")
									? "锁定状态未确认"
									: "部分状态缺失";
		} else {
			status = "success";
			summary = !volumes.length
				? "无可处理的卷"
				: scope === "unlock"
					? volumes.every((v) => v.lock === "已解锁")
						? "已解锁"
						: "请求已返回"
					: volumes.every((v) => v.conversionCode === 0)
						? "已完全解密"
						: volumes.some((v) => v.conversionCode === 3)
							? "解密中"
							: "请求已返回";
		}
	}
	const fields = volumes.flatMap((v) => [
		field("目标卷", `${v.drive} · ${v.id}`),
		field("转换状态", `${v.conversion}${v.conversionCode === null ? "" : ` (${v.conversionCode})`}`),
		field("锁定状态", v.lock),
		field("请求结果", v.operation || "未知"),
		field("说明", v.reason),
		field("采集时刻", source?.collectedAt),
	]);
	return {
		...base,
		kind,
		status,
		title: scope === "unlock" ? `解锁 ${volumes[0]?.drive ?? drive}` : "关闭 BitLocker",
		summary,
		scope,
		volumes,
		fields,
		errors,
	};
}
