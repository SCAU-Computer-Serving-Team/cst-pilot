import type { Dirent, Stats } from "node:fs";
import { existsSync, promises as fsp } from "node:fs";
import { join, resolve } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { diagnosticResult, OUTPUT_GUIDELINE } from "./result.ts";
import { errorMessage } from "./runtime.ts";

/**
 * ls - 自定义目录浏览工具（覆盖 pi 内置 ls）
 *
 * pi 内置 ls 的先天缺陷：stat 拿到了文件大小却在输出时丢弃。
 * 本工具以同名覆盖内置（pi 的 toolRegistry 中扩展工具后注册即覆盖内置同名工具）：
 *   - 列出目录的【直接子项】（文件 + 子文件夹），按占用大小倒序
 *   - 文件给字节；文件夹给递归聚合大小
 *   - 附各子项占父目录的百分比
 *   - 默认截断最大的前 20 项，并报告被截掉的数量与合计（模型不丢失全貌感）
 *
 * 大小引擎：逐文件递归累计（50 万条目、30 秒预算，超限标注"下界"），结构只读。
 */

const TOP_DEFAULT = 20;
const TOP_MAX = 50;
const WALK_BUDGET = 500000; // 递归熔断：最多 stat 的文件数
const WALK_TIMEOUT_MS = 30000; // 递归熔断：总时长

/* ---------------- 大小格式化 ---------------- */

function formatBytes(bytes: number): string {
	if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
	if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
	if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	return `${bytes} B`;
}

/* ---------------- 降级路径：递归累计 ---------------- */

async function walkSize(
	dir: string,
	budget: { files: number; deadline: number },
	signal?: AbortSignal,
): Promise<{ bytes: number; complete: boolean }> {
	signal?.throwIfAborted();
	if (budget.files >= WALK_BUDGET || Date.now() >= budget.deadline) return { bytes: 0, complete: false };
	let bytes = 0;
	let complete = true;
	let entries: Dirent[] | undefined;
	try {
		entries = await fsp.readdir(dir, { withFileTypes: true });
	} catch {
		signal?.throwIfAborted();
		return { bytes: 0, complete: false };
	}
	for (const e of entries) {
		signal?.throwIfAborted();
		if (budget.files >= WALK_BUDGET || Date.now() >= budget.deadline) return { bytes, complete: false };
		budget.files++;
		if (e.isSymbolicLink()) {
			complete = false;
			continue;
		}
		const full = join(dir, e.name);
		try {
			if (e.isDirectory()) {
				const sub = await walkSize(full, budget, signal);
				bytes += sub.bytes;
				complete = complete && sub.complete;
			} else {
				bytes += (await fsp.stat(full)).size;
			}
		} catch {
			signal?.throwIfAborted();
			complete = false;
		}
	}
	return { bytes, complete };
}

/* ---------------- 工具定义 ---------------- */

export default function registerLs(pi: ExtensionAPI) {
	pi.registerTool({
		name: "ls",
		label: "LS",
		description:
			"列出目录的直接子项（文件和子文件夹），按占用大小从大到小排序，默认返回最大的前 20 项。" +
			"文件显示字节大小，子文件夹显示递归聚合大小，每项附带占父目录的百分比。" +
			"若被截断，会报告剩余项数与合计。用于浏览任意目录的内容与体积分布。" +
			" 输出 JSON 最多 50 KiB，超限标注 outputTruncated；请缩小查询范围获取省略内容。",
		promptSnippet: "List directory contents with sizes and percentages (read-only)",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Use ls to browse a directory's size distribution; use disk scope=usage for full-tree analysis of large paths.",
		],
		parameters: Type.Object({
			path: Type.String({ description: "要浏览的目录路径" }),
			top: Type.Optional(Type.Number({ description: `返回前 N 项（默认 ${TOP_DEFAULT}，最大 ${TOP_MAX}）` })),
		}),
		async execute(_toolCallId, params, signal, _onUpdate, ctx) {
			signal?.throwIfAborted();
			const target = resolve(ctx.cwd, params.path);
			const top = Math.max(1, Math.min(TOP_MAX, Math.floor(params.top ?? TOP_DEFAULT)));
			if (!existsSync(target)) {
				throw new Error(`路径不存在: ${target}`);
			}
			let st: Stats | undefined;
			try {
				st = await fsp.stat(target);
			} catch (e) {
				signal?.throwIfAborted();
				throw new Error(`无法读取: ${errorMessage(e)}`);
			}
			if (!st.isDirectory()) {
				throw new Error(`不是目录: ${target}`);
			}

			let names: string[];
			try {
				names = await fsp.readdir(target);
			} catch (e) {
				signal?.throwIfAborted();
				throw new Error(`无法列出: ${errorMessage(e)}`);
			}

			let incomplete = false;
			const items: { name: string; type: "file" | "dir" | "unknown"; bytes: number | null }[] = [];
			const budget = { files: 0, deadline: Date.now() + WALK_TIMEOUT_MS };

			for (const name of names) {
				signal?.throwIfAborted();
				const full = join(target, name);
				try {
					const s = await fsp.stat(full);
					if (s.isDirectory()) {
						const r = await walkSize(full, budget, signal);
						items.push({ name, type: "dir", bytes: r.bytes });
						if (!r.complete) incomplete = true;
					} else {
						items.push({ name, type: "file", bytes: s.size });
					}
				} catch {
					signal?.throwIfAborted();
					const s = await fsp.stat(full).catch(() => null);
					if (s?.isDirectory()) {
						const r = await walkSize(full, budget, signal);
						items.push({ name, type: "dir", bytes: r.bytes });
						if (!r.complete) incomplete = true;
					} else if (s) {
						items.push({ name, type: "file", bytes: s.size });
					} else {
						items.push({ name, type: "unknown", bytes: null });
						incomplete = true;
					}
				}
			}

			items.sort((a, b) => (b.bytes ?? -1) - (a.bytes ?? -1));
			const totalBytes = items.reduce((sum, i) => sum + (i.bytes ?? 0), 0);
			const truncated = items.length > top;
			const kept = items.slice(0, top);
			const omitted = items.slice(top);
			const omittedBytes = omitted.reduce((sum, i) => sum + (i.bytes ?? 0), 0);

			signal?.throwIfAborted();
			return diagnosticResult({
				path: target,
				totalChildren: items.length,
				totalSize: formatBytes(totalBytes),
				method: "walk",
				entries: kept.map((i) => ({
					name: i.name,
					type: i.type,
					size: i.bytes === null ? null : formatBytes(i.bytes),
					bytes: i.bytes,
					pct:
						!incomplete && i.bytes !== null && totalBytes > 0 ? +((100 * i.bytes) / totalBytes).toFixed(1) : null,
				})),
				...(truncated
					? {
							omitted: {
								count: omitted.length,
								unknownCount: omitted.filter((i) => i.bytes === null).length,
								size: formatBytes(omittedBytes),
								note: "已按大小截断，其余为小项",
							},
						}
					: {}),
				...(incomplete ? { notice: "部分子项大小未统计完整（熔断或无权限），所示大小为下界" } : {}),
			});
		},
	});
}
