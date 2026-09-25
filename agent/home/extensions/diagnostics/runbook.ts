import { mkdir, readdir, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { diagnosticResult, OUTPUT_GUIDELINE } from "./result.ts";
import { errorMessage, ROOT_DIR } from "./runtime.ts";

/**
 * runbook - 命令交付工具（cst-pilot 定制）
 *
 * 诊断只读，修复动作只能由队员手动执行。本工具把待执行的命令写成 txt，
 * 落在工具包 outbox\<日期>\ 下，队员打开文件逐条复制，不必从聊天记录里翻找。
 *
 * 边界：
 * - 只写工具包自身的 outbox 目录，不碰机主文件、注册表与系统配置。
 * - 只写 .txt，不生成 .bat/.cmd/.ps1，避免双击即执行。
 * - 一次调用只产出一个文件；命令按风险分档，同一档合并成一份清单。
 * - 序号是「一次生成」的编号，当天唯一递增；沿用序号重写就是修正那一份。
 * - 版式由本工具拼装，模型只提供字段，避免 Markdown 与格式漂移。
 *
 * 需求见 doc/PRD.md「命令交付需求」（D1–D12）；命令内容规范见 skills/runbook。
 */

const OUTBOX_DIR = join(ROOT_DIR, "outbox");
const MAX_ITEMS = 20;
const MAX_TITLE = 48;
const MAX_SUMMARY = 300;
const MAX_COMMAND = 2000;

/** 文件名：<序号>-<风险档>-<标题>.txt */
const NAME_RE = /^(\d{2,3})-(安全|较安全|需确认)-(.+)\.txt$/;

/** 风险分档：文件名用中文档名，便于队员一眼分辨。 */
const LEVELS = {
	safe: "安全",
	low: "较安全",
	confirm: "需确认",
} as const;
type Level = keyof typeof LEVELS;

const SHELLS = { cmd: "cmd", powershell: "PowerShell" } as const;
type Shell = keyof typeof SHELLS;

type RunbookItem = {
	summary: string;
	command: string;
	shell: Shell;
	admin?: boolean;
};

type RunbookParams = {
	title: string;
	level: string;
	items: readonly RunbookItem[];
	sequence?: number;
};

function pad(value: number): string {
	return String(value).padStart(2, "0");
}

function localStamp(date: Date): { dir: string; text: string } {
	const day = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
	return { dir: day, text: `${day} ${pad(date.getHours())}:${pad(date.getMinutes())}` };
}

/** 文件名主体：去掉 Windows 非法字符与控制字符，压掉连续空白。 */
function cleanTitle(raw: string): string {
	const cleaned = raw
		.replace(/[\\/:*?"<>|\u0000-\u001f]/g, "")
		.replace(/\s+/g, " ")
		.trim();
	return (cleaned || "命令清单").slice(0, MAX_TITLE);
}

function assertText(value: string, field: string, max: number): string {
	const text = value.trim();
	if (!text) throw new Error(`${field}不能为空`);
	if (/[\r\n]/.test(text)) {
		throw new Error(`${field}不能包含换行；多行操作请用分号合并成一条命令`);
	}
	if (text.length > max) throw new Error(`${field}超过 ${max} 字符，请精简或拆成多条`);
	return text;
}

function shellLabel(item: RunbookItem): string {
	return item.admin ? `${SHELLS[item.shell]}（管理员）` : `${SHELLS[item.shell]}（普通权限）`;
}

/** 当天目录里的清单文件与修改时间，用于分配批次和替换同档清单。 */
async function readEntries(dir: string): Promise<{ name: string; mtimeMs: number }[]> {
	let names: string[];
	try {
		names = await readdir(dir);
	} catch {
		return [];
	}
	const entries: { name: string; mtimeMs: number }[] = [];
	for (const name of names) {
		if (!NAME_RE.test(name)) continue;
		const info = await stat(join(dir, name)).catch(() => null);
		if (info) entries.push({ name, mtimeMs: info.mtimeMs });
	}
	return entries;
}

/**
 * 序号是「一次生成」的编号：当天目录里已用的最大序号 +1，跨天重置。
 * 序号当天唯一且递增，调用方只能在修正已有清单时用 sequence 指定具体值。
 */
async function nextSequence(dir: string): Promise<number> {
	const entries = await readEntries(dir);
	let max = 0;
	for (const entry of entries) {
		const matched = NAME_RE.exec(entry.name);
		if (matched) max = Math.max(max, Number(matched[1]));
	}
	return max + 1;
}

/** 写入一份清单：一个序号只对应一份文件，沿用序号重写就是修正。 */
async function writeSlot(
	dir: string,
	sequence: number,
	level: Level,
	title: string,
	body: string,
): Promise<{ file: string; name: string }> {
	const label = LEVELS[level];
	for (const entry of await readEntries(dir)) {
		const matched = NAME_RE.exec(entry.name);
		if (matched && Number(matched[1]) === sequence) {
			await rm(join(dir, entry.name), { force: true });
		}
	}
	const name = `${pad(sequence)}-${label}-${title}.txt`;
	const file = join(dir, name);
	await writeFile(file, body, { encoding: "utf8" });
	return { file, name };
}

/**
 * 模型可能在同一轮回复里并行调用多个 runbook（按风险档各写一份）。
 * 序号分配必须串行，否则并发调用会读到同一个目录快照、拿到同一个序号。
 */
const queueKey = Symbol.for("cst-pilot.runbook.write-queue");
type QueueState = { pending: Promise<unknown> };

function serialize<T>(task: () => Promise<T>): Promise<T> {
	// Every Web session loads its own diagnostics extension. A module-local queue
	// would let two sessions choose the same next sequence from one directory.
	const globals = globalThis as typeof globalThis & { [queueKey]?: QueueState };
	if (!globals[queueKey]) globals[queueKey] = { pending: Promise.resolve() };
	const queue = globals[queueKey];
	const result = queue.pending.then(task, task);
	queue.pending = result.then(
		() => undefined,
		() => undefined,
	);
	return result;
}

/** 版式：文件头一行概括，条目为环境 / 说明 / 命令三段，命令独占一行。 */
function buildBody(title: string, level: Level, items: RunbookItem[], timeText: string): string {
	const lines = [`${title}（${LEVELS[level]}） ${timeText}`, ""];
	for (const item of items) {
		lines.push(`环境：${shellLabel(item)}`);
		lines.push(`说明：${item.summary.trim()}`);
		lines.push("命令：");
		lines.push(item.command.trim());
		lines.push("");
	}
	// UTF-8 BOM + CRLF：中文系统记事本直接可读，双击只打开不会执行
	return `\uFEFF${lines.join("\r\n")}`;
}

async function deliver(params: RunbookParams): Promise<Record<string, unknown>> {
	const level = params.level as Level;
	if (!(level in LEVELS)) throw new Error(`level 只能是 ${Object.keys(LEVELS).join(" / ")}`);
	if (!params.items.length) throw new Error("至少需要一条命令");
	if (params.sequence !== undefined && (!Number.isInteger(params.sequence) || params.sequence < 1)) {
		throw new Error("sequence 必须是 >= 1 的整数");
	}
	const title = cleanTitle(assertText(params.title, "title", MAX_TITLE));
	const items = params.items.map((item) => {
		if (!(item.shell in SHELLS)) throw new Error("shell 只能是 cmd / powershell");
		return {
			...item,
			summary: assertText(item.summary, "说明", MAX_SUMMARY),
			command: assertText(item.command, "命令", MAX_COMMAND),
		};
	});

	const stamp = localStamp(new Date());
	const dir = join(OUTBOX_DIR, stamp.dir);
	const body = buildBody(title, level, items, stamp.text);
	try {
		const written = await serialize(async () => {
			await mkdir(dir, { recursive: true });
			const sequence = params.sequence ?? (await nextSequence(dir));
			const { file, name } = await writeSlot(dir, sequence, level, title, body);
			return { sequence, file, name };
		});
		return {
			runbook: {
				file: written.file,
				dir,
				name: written.name,
				sequence: written.sequence,
				level,
				levelLabel: LEVELS[level],
				items: items.length,
				bytes: Buffer.byteLength(body, "utf8"),
				encoding: "utf-8-bom",
				notice:
					`清单只写工具包 outbox，未改动机主系统。本次序号 ${written.sequence}（当天唯一递增）。` +
					`队员要改这一份命令时，沿用 sequence=${written.sequence} 重写即可，不会新增文件。` +
					"文件是纯文本，不会自动执行；请把 file 路径原样转述给队员，队员打开后逐条复制命令。",
			},
		};
	} catch (error) {
		throw new Error(`命令清单写入失败：${errorMessage(error)}`);
	}
}

/* ------------------------------------------------------------------ */

export default function registerRunbook(pi: ExtensionAPI) {
	pi.registerTool({
		name: "runbook",
		label: "Command Runbook",
		description:
			"把需要队员手动执行的命令写成 txt 清单，落在工具包 outbox\\<日期>\\ 下（只写工具包，不动机主系统，不执行任何命令）。" +
			"一次调用产出一个文件，序号当天唯一递增；命令按风险分档（safe=安全 / low=较安全 / confirm=需与机主确认），同一档合并成一份清单。" +
			"要改已经写好的某一份，用它的 sequence 重写，不会新增文件。" +
			"每条命令必须是一行、可整行复制，并带一句说明功能与影响。" +
			"中等及以上风险、不可逆且无退路的操作不要写入任何清单。" +
			" 输出 JSON 最多 50 KiB，超限标注 outputTruncated。",
		promptSnippet: "Write a copy-paste command runbook (txt) into the toolkit outbox for the member to run manually",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Use runbook when you are about to hand the member commands to run by hand: cleanup, uninstall, service or startup changes, driver removal.",
			"Call it once per risk level, not once per command: merge similar operations into one command and put every command of the same level in the same file.",
			"Sequences are unique and increasing within a day. To revise a runbook you already wrote, call again with its sequence; omit sequence to get a new one.",
			"Do not put medium-or-higher risk or irreversible operations into a runbook; explain those in chat instead. Read skills/runbook/SKILL.md before the first call.",
			"Always repeat the returned file path verbatim; the member locates the file from the toolkit outbox folder.",
		],
		parameters: Type.Object({
			title: Type.String({ description: "清单标题，进入文件名，如「清理 C 盘临时文件」" }),
			level: Type.Union([Type.Literal("safe"), Type.Literal("low"), Type.Literal("confirm")], {
				description: "风险档：safe=安全，low=较安全，confirm=需与机主确认",
			}),
			sequence: Type.Optional(
				Type.Integer({
					minimum: 1,
					description: "沿用已有序号修正某一份清单时传入返回值里的 sequence；省略则分配当天递增的新序号",
				}),
			),
			items: Type.Array(
				Type.Object({
					summary: Type.String({ description: "一句话说明这条命令做什么、有什么影响" }),
					command: Type.String({ description: "命令正文，必须单行、无 Markdown 标记" }),
					shell: Type.Union([Type.Literal("cmd"), Type.Literal("powershell")], {
						description: "命令在哪个解释器里执行",
					}),
					admin: Type.Optional(Type.Boolean({ description: "是否需要管理员权限，默认否" })),
				}),
				{ minItems: 1, maxItems: MAX_ITEMS },
			),
		}),

		async execute(_toolCallId, params, signal, _onUpdate, _ctx) {
			signal?.throwIfAborted();
			const result = await deliver(params);
			signal?.throwIfAborted();
			return diagnosticResult(result);
		},
	});
}
