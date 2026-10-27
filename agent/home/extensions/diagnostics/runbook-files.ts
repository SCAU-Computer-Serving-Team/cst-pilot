import { createHash, randomUUID } from "node:crypto";
import { mkdir, readdir, realpath, rename, rm, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { join } from "node:path";
import { errorMessage, ROOT_DIR } from "./runtime.ts";

/**
 * runbook - 命令交付工具（cst-pilot 定制）
 *
 * 本工具只交付需由队员手动执行的修复命令，把待执行的命令写成 txt，
 * 落在工具包 outbox\<日期>\ 下，队员打开文件逐条复制，不必从聊天记录里翻找。
 *
 * 边界：
 * - 只写工具包自身的 outbox 目录，不碰机主文件、注册表与系统配置。
 * - 只写 .txt，不生成 .bat/.cmd/.ps1，避免双击即执行。
 * - 一次调用只产出一个文件；命令按风险分档，同一档合并成一份清单。
 * - 序号是「一次生成」的编号，当天唯一递增；沿用序号重写就是修正那一份。
 * - 版式由本工具拼装，模型只提供字段，避免 Markdown 与格式漂移。
 *
 * 需求见 doc/SPEC.md「命令交付需求」与 doc/diagnostics/SPEC.md；命令内容规范见 skills/runbook。
 */

export const OUTBOX_DIR = join(ROOT_DIR, "outbox");
const MAX_ITEMS = 20;
const MAX_TITLE = 48;
const MAX_SUMMARY = 300;
const MAX_COMMAND = 2000;

/** 文件名：<序号>-<风险档>-<标题>.txt */
const NAME_RE = /^(\d{2,3})-(安全|较安全|需确认)-(.+)\.txt$/;

/** 风险分档：文件名用中文档名，便于队员一眼分辨。 */
export const LEVELS = {
	safe: "安全",
	low: "较安全",
	confirm: "需确认",
} as const;
export type Level = keyof typeof LEVELS;

const SHELLS = { cmd: "cmd", powershell: "PowerShell" } as const;
type Shell = keyof typeof SHELLS;

export type RunbookItem = {
	summary: string;
	command: string;
	shell: Shell;
	admin?: boolean;
};

export type RunbookParams = {
	title: string;
	level: string;
	items: readonly RunbookItem[];
	sequence?: number;
};

function pad(value: number): string {
	return String(value).padStart(2, "0");
}

export function localStamp(date: Date): { dir: string; text: string } {
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
	if (typeof value !== "string") throw new Error(`${field}必须是文本`);
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
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
		throw error;
	}
	const entries: { name: string; mtimeMs: number }[] = [];
	for (const name of names) {
		if (!NAME_RE.test(name)) continue;
		const info = await stat(join(dir, name)).catch((error: NodeJS.ErrnoException) => {
			if (error.code === "ENOENT") return undefined;
			throw error;
		});
		if (info?.isFile()) entries.push({ name, mtimeMs: info.mtimeMs });
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
	replacing: boolean,
): Promise<{ file: string; name: string }> {
	const previous = (await readEntries(dir)).filter((entry) => Number(NAME_RE.exec(entry.name)?.[1]) === sequence);
	if (replacing && previous.length === 0) throw new Error(`序号 ${sequence} 不存在，无法修正`);
	if (previous.length > 1 || (!replacing && previous.length))
		throw new Error(`序号 ${sequence} 已重复，请检查清单目录`);
	const name = `${pad(sequence)}-${LEVELS[level]}-${title}.txt`;
	const file = join(dir, name);
	const temporary = join(dir, `.${randomUUID()}.txt`);
	try {
		await writeFile(temporary, body, { encoding: "utf8", flag: "wx" });
		await rename(temporary, file);
		const old = previous[0]?.name;
		const sameName = old && (process.platform === "win32" ? old.toLowerCase() === name.toLowerCase() : old === name);
		if (old && !sameName) {
			try {
				await rm(join(dir, old));
			} catch (error) {
				await rm(file);
				throw error;
			}
		}
		return { file, name };
	} finally {
		await rm(temporary, { force: true });
	}
}

/** 协调同一清单目录的独立进程；进程退出后由内核释放，不落锁文件。 */
export async function withDirectoryLock<T>(dir: string, task: () => Promise<T>): Promise<T> {
	const canonical = await realpath(dir);
	const key = createHash("sha256")
		.update(process.platform === "win32" ? canonical.toLowerCase() : canonical)
		.digest();
	const lock = createServer((socket) => socket.destroy());
	const address = process.platform === "win32" ? `\\\\.\\pipe\\cst-pilot-runbook-${key.toString("hex")}` : undefined;
	await new Promise<void>((resolve, reject) => {
		lock.once("error", (error: NodeJS.ErrnoException) => {
			const occupied =
				error.code === "EADDRINUSE" ||
				(!!address && error.code === "ERR_INVALID_ARG_TYPE" && error.message === `Failed to listen at ${address}`);
			reject(occupied ? new Error("清单目录正由另一进程写入，请稍后重试") : error);
		});
		if (address) lock.listen(address, resolve);
		else lock.listen({ host: "127.0.0.1", port: 32000 + (key.readUInt32BE(0) % 20000), exclusive: true }, resolve);
	});
	lock.unref();
	try {
		return await task();
	} finally {
		await new Promise<void>((resolve, reject) => lock.close((error) => (error ? reject(error) : resolve())));
	}
}

/**
 * 模型可能在同一轮回复里并行调用多个 runbook（按风险档各写一份）。
 * 序号分配必须串行，否则并发调用会读到同一个目录快照、拿到同一个序号。
 */
const queueKey = Symbol.for("cst-pilot.runbook.write-queue");
type QueueState = { pending: Promise<unknown> };

export function serialize<T>(task: () => Promise<T>): Promise<T> {
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
export function buildBody(title: string, level: Level, items: RunbookItem[], timeText: string): string {
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

export function validateRunbook(params: RunbookParams): { title: string; level: Level; items: RunbookItem[] } {
	const level = params.level as Level;
	if (!Object.hasOwn(LEVELS, level)) throw new Error(`level 只能是 ${Object.keys(LEVELS).join(" / ")}`);
	if (!Array.isArray(params.items) || !params.items.length || params.items.length > MAX_ITEMS)
		throw new Error("命令数量必须是 1–20 条");
	if (
		params.sequence !== undefined &&
		(!Number.isInteger(params.sequence) || params.sequence < 1 || params.sequence > 999)
	) {
		// 序号与 NAME_RE（\d{2,3}）配套：超过 999 的文件名不被识别，重写不会替换旧文件，
		// 违反 D2「沿用序号重写即修正，不新增文件」，因此拒绝。
		throw new Error("sequence 必须是 1-999 的整数");
	}
	const title = cleanTitle(assertText(params.title, "title", MAX_TITLE));
	const items = params.items.map((item) => {
		if (!item || !Object.hasOwn(SHELLS, item.shell)) throw new Error("shell 只能是 cmd / powershell");
		if (item.admin !== undefined && typeof item.admin !== "boolean") throw new Error("admin 必须是布尔值");
		return {
			shell: item.shell,
			admin: item.admin === true,
			summary: assertText(item.summary, "说明", MAX_SUMMARY),
			command: assertText(item.command, "命令", MAX_COMMAND),
		};
	});

	return { title, level, items };
}

export async function deliverRunbook(params: RunbookParams): Promise<Record<string, unknown>> {
	const { title, level, items } = validateRunbook(params);
	const stamp = localStamp(new Date());
	const dir = join(OUTBOX_DIR, stamp.dir);
	const body = buildBody(title, level, items, stamp.text);
	try {
		const written = await serialize(async () => {
			await mkdir(dir, { recursive: true });
			return withDirectoryLock(dir, async () => {
				const sequence = params.sequence ?? (await nextSequence(dir));
				if (sequence > 999) throw new Error("当天序号已达到 999，请在次日生成新清单");
				const { file, name } = await writeSlot(dir, sequence, level, title, body, params.sequence !== undefined);
				return { sequence, file, name };
			});
		});
		return {
			runbook: {
				title,
				file: written.file,
				dir,
				name: written.name,
				sequence: written.sequence,
				action: params.sequence === undefined ? "created" : "replaced",
				level,
				levelLabel: LEVELS[level],
				items: items.length,
				commands: items.map((item) => ({
					summary: item.summary,
					command: item.command,
					shell: SHELLS[item.shell],
					admin: item.admin === true,
				})),
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
