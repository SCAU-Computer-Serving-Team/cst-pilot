import { createHash, randomUUID } from "node:crypto";
import { constants } from "node:fs";
import { copyFile, mkdir, readdir, readFile, realpath, rename, rm, stat, writeFile } from "node:fs/promises";
import { basename, isAbsolute, join, relative, sep } from "node:path";
import {
	artifactDate,
	artifactDirectory,
	artifactName,
	ensureArtifactDirectory,
	existingArtifactDirectory,
} from "../../../diagnostics/artifact-paths.ts";
import type { ExportSnapshot, SessionExport } from "../../shared/exports.ts";
import type { ExportRenderer } from "./render.ts";
import { ExportError, exportOptions } from "./snapshot.ts";

type RecordFile = { name: string; hash: string };
type ExportRecord = { result: SessionExport; stored: RecordFile[] };
type Work = { record: ExportRecord; snapshot: ExportSnapshot; visible: SessionExport };
const VALID_ID = /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/;

function exportBase(snapshot: ExportSnapshot): string {
	const time = new Date(snapshot.capturedAt);
	const clock = [time.getHours(), time.getMinutes(), time.getSeconds()]
		.map((value) => String(value).padStart(2, "0"))
		.join("");
	return `${artifactName(snapshot.title)}-${snapshot.options.date.replaceAll("-", "")}-${clock}`;
}

/** 文件登记与作业队列归后端所有；sessions 目录只存最终 PNG / HTML。 */
export class SessionExportStore {
	private readonly root: string;
	private readonly metadata: string;
	private readonly renderer: ExportRenderer;
	private readonly queue: Work[] = [];
	private active?: Work;
	private controller?: AbortController;
	private pumping?: Promise<void>;
	private closing = false;
	private reservations = 0;
	private revision = 0;
	private readonly creating = new Set<Promise<SessionExport>>();
	private shutdown?: Promise<void>;
	constructor(root: string, metadata: string, renderer: ExportRenderer) {
		this.root = root;
		this.metadata = metadata;
		this.renderer = renderer;
	}
	private async save(record: ExportRecord): Promise<void> {
		await mkdir(this.metadata, { recursive: true });
		const temporary = join(this.metadata, `${record.result.id}.${randomUUID()}.tmp`);
		try {
			await writeFile(temporary, JSON.stringify(record), { flag: "wx" });
			await rename(temporary, join(this.metadata, `${record.result.id}.json`));
		} finally {
			await rm(temporary, { force: true });
		}
	}
	create(source: ExportSnapshot): Promise<SessionExport> {
		const task = this.enqueue(source);
		this.creating.add(task);
		void task.then(
			() => this.creating.delete(task),
			() => this.creating.delete(task),
		);
		return task;
	}
	private async commit(work: Work): Promise<void> {
		await this.save(work.record);
		work.visible = structuredClone(work.record.result);
	}
	private async enqueue(source: ExportSnapshot): Promise<SessionExport> {
		if (this.closing) throw new ExportError(503, "export_stopping", "程序正在退出，请稍后导出");
		if (this.queue.length + this.reservations + Number(!!this.active) >= 8)
			throw new ExportError(429, "export_busy", "导出任务较多，请等待已有任务完成");
		this.reservations++;
		try {
			const snapshot = structuredClone(source);
			const id = randomUUID();
			const result: SessionExport = {
				id,
				sessionId: snapshot.sessionId,
				title: snapshot.title,
				capturedAt: snapshot.capturedAt,
				options: snapshot.options,
				status: "queued",
				files: [],
				directory: artifactDirectory(this.root, snapshot.options.date, "sessions"),
			};
			const work: Work = { record: { result, stored: [] }, snapshot, visible: structuredClone(result) };
			await this.save(work.record);
			if (this.closing) {
				await rm(join(this.metadata, `${id}.json`), { force: true });
				throw new ExportError(503, "export_stopping", "程序正在退出，请稍后导出");
			}
			this.queue.push(work);
			this.revision++;
			const response = structuredClone(result);
			this.start();
			return response;
		} catch (cause) {
			if (cause instanceof ExportError) throw cause;
			const full = cause && typeof cause === "object" && "code" in cause && cause.code === "ENOSPC";
			throw new ExportError(
				full ? 507 : 500,
				full ? "storage_full" : "export_record_unavailable",
				full ? "工具包存储空间不足，请清理后重试" : "导出任务无法保存，请检查工具包写入权限",
			);
		} finally {
			this.reservations--;
		}
	}
	private start(): void {
		this.pumping ??= this.pump().finally(() => {
			this.pumping = undefined;
			if (this.queue.length) this.start();
		});
	}
	private async pump(): Promise<void> {
		while (this.queue.length) {
			const work = this.queue.shift();
			if (!work) break;
			this.active = work;
			await this.run(work);
			this.active = undefined;
			this.revision++;
		}
	}
	private async run(work: Work): Promise<void> {
		const { result } = work.record;
		const published: string[] = [];
		const staging = join(this.metadata, "work", result.id);
		this.controller = new AbortController();
		const timeout = setTimeout(
			() => this.controller?.abort(new ExportError(504, "export_timeout", "导出超时，请重试或缩短当前分支")),
			120_000,
		);
		try {
			if (this.closing) throw new ExportError(503, "export_interrupted", "导出已随程序退出取消，请重新导出");
			result.status = "rendering";
			await this.commit(work);
			await mkdir(staging, { recursive: true });
			const files: { file: string; name: string; hash: string; bytes: number }[] = [];
			let bytes = 0;
			await this.renderer.render(
				work.snapshot,
				async (extension, data) => {
					this.controller?.signal.throwIfAborted();
					bytes += data.length;
					if (
						extension !== result.options.format ||
						files.length >= (extension === "html" ? 1 : 64) ||
						bytes > 200 * 1024 * 1024 ||
						!data.length
					)
						throw new ExportError(413, "export_too_large", "产物超过大小限制，请缩短当前分支后重试");
					const name = `${String(files.length + 1).padStart(2, "0")}.${extension}`;
					const file = join(staging, name);
					await writeFile(file, data, { flag: "wx" });
					files.push({ file, name, hash: createHash("sha256").update(data).digest("hex"), bytes: data.length });
				},
				this.controller.signal,
			);
			this.controller.signal.throwIfAborted();
			if (!files.length) throw new Error("导出未生成文件");
			const directory = await ensureArtifactDirectory(this.root, result.options.date, "sessions");
			const base = exportBase(work.snapshot);
			for (let group = 1; ; group++) {
				this.controller.signal.throwIfAborted();
				const stem = `${base}${group === 1 ? "" : `-${group}`}`;
				try {
					for (const [index, file] of files.entries()) {
						this.controller.signal.throwIfAborted();
						const page = result.options.format === "png" ? `-${String(index + 1).padStart(2, "0")}` : "";
						file.name = `${stem}${page}.${result.options.format}`;
						const target = join(directory, file.name);
						await copyFile(file.file, target, constants.COPYFILE_EXCL);
						published.push(target);
					}
					break;
				} catch (cause) {
					if (!cause || typeof cause !== "object" || !("code" in cause) || cause.code !== "EEXIST") throw cause;
					// 只撤回本次尝试创建的文件，整组换号，保留已有分图。
					await Promise.all(published.map((file) => rm(file, { force: true })));
					published.length = 0;
				}
			}
			this.controller.signal.throwIfAborted();
			work.record.stored = files.map(({ name, hash }) => ({ name, hash }));
			result.files = files.map(({ name, bytes }, index) => ({
				name,
				bytes,
				url: `/api/exports/${result.id}/files/${index}`,
			}));
			result.status = "completed";
			await this.commit(work);
		} catch (cause) {
			await Promise.all(published.map((file) => rm(file, { force: true }).catch(() => undefined)));
			result.status = "failed";
			result.files = [];
			work.record.stored = [];
			const diskCode = cause && typeof cause === "object" && "code" in cause ? cause.code : "";
			const reason = this.controller.signal.aborted ? this.controller.signal.reason : cause;
			result.error =
				reason instanceof ExportError
					? { code: reason.code, message: reason.message }
					: {
							code: diskCode === "ENOSPC" ? "storage_full" : "export_failed",
							message: this.controller.signal.aborted
								? "导出超时或已取消，请重试或缩短当前分支"
								: diskCode === "ENOSPC"
									? "工具包存储空间不足，请清理后重试"
									: "导出未完成，请检查浏览器和工具包写入权限后重试",
						};
			await this.save(work.record).catch(() => undefined);
			work.visible = structuredClone(result);
		} finally {
			clearTimeout(timeout);
			this.controller = undefined;
			await rm(staging, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 }).catch(() => undefined);
		}
	}
	private live(id: string): ExportRecord | undefined {
		const work =
			this.active?.record.result.id === id ? this.active : this.queue.find((item) => item.record.result.id === id);
		return work ? { result: structuredClone(work.visible), stored: structuredClone(work.record.stored) } : undefined;
	}
	private async read(id: string): Promise<ExportRecord> {
		if (!VALID_ID.test(id)) throw new ExportError(404, "export_not_found", "导出记录不存在");
		const live = this.live(id);
		if (live) return live;
		const revision = this.revision;
		try {
			const record = JSON.parse(await readFile(join(this.metadata, `${id}.json`), "utf8")) as ExportRecord;
			const current = this.live(id);
			if (current) return current;
			if (revision !== this.revision) return this.read(id);
			if (
				record.result.id !== id ||
				!Array.isArray(record.stored) ||
				!Array.isArray(record.result.files) ||
				!["queued", "rendering", "completed", "failed"].includes(record.result.status)
			)
				throw new Error("invalid metadata");
			const { format, theme, includeThinking, includeTools, date } = record.result.options;
			exportOptions({ format, theme, includeThinking, includeTools });
			artifactDate(date);
			if (record.result.status === "queued" || record.result.status === "rendering") {
				record.result.status = "failed";
				record.result.error = { code: "export_interrupted", message: "上次导出未完成，请重新导出" };
				await this.save(record);
			}
			return record;
		} catch (cause) {
			if (cause && typeof cause === "object" && "code" in cause && cause.code === "ENOENT")
				throw new ExportError(404, "export_not_found", "导出记录不存在");
			throw new ExportError(503, "export_record_unavailable", "导出记录无法读取，请检查工具包所在介质");
		}
	}
	async get(id: string): Promise<SessionExport> {
		return (await this.read(id)).result;
	}
	async list(sessionId: string): Promise<SessionExport[]> {
		const names = await readdir(this.metadata).catch((error: NodeJS.ErrnoException) => {
			if (error.code === "ENOENT") return [];
			throw error;
		});
		const results: SessionExport[] = [];
		for (const name of names) {
			if (!name.endsWith(".json") || !VALID_ID.test(name.slice(0, -5))) continue;
			const result = await this.get(name.slice(0, -5));
			if (result.sessionId === sessionId) results.push(result);
		}
		return results.sort((a, b) => b.capturedAt.localeCompare(a.capturedAt)).slice(0, 50);
	}
	async directory(id: string): Promise<string> {
		const { result } = await this.read(id);
		if (result.status !== "completed")
			throw new ExportError(409, "export_not_completed", "导出尚未保存完成，请完成后打开目录");
		try {
			const directory = await realpath(await existingArtifactDirectory(this.root, result.options.date, "sessions"));
			if (!(await stat(directory)).isDirectory())
				throw new ExportError(404, "export_directory_missing", "保存目录已不存在，请重新导出");
			return directory;
		} catch (cause) {
			if (cause instanceof ExportError) throw cause;
			const code = cause && typeof cause === "object" && "code" in cause ? cause.code : undefined;
			if (code === "ENOENT" || code === "ENOTDIR")
				throw new ExportError(404, "export_directory_missing", "保存目录已不存在，请重新导出");
			if (code === "EACCES" || code === "EPERM")
				throw new ExportError(403, "export_directory_denied", "无法访问保存目录，请检查目录权限");
			if (!code) throw new ExportError(403, "export_directory_invalid", "保存目录指向工具包外，无法打开");
			throw new ExportError(503, "export_directory_unavailable", "保存目录无法读取，请检查工具包所在介质");
		}
	}
	async file(id: string, index: number): Promise<{ name: string; data: Buffer; type: string }> {
		const record = await this.read(id);
		const entry = record.stored[index];
		if (
			record.result.status !== "completed" ||
			!Number.isSafeInteger(index) ||
			index < 0 ||
			!entry ||
			basename(entry.name) !== entry.name
		)
			throw new ExportError(404, "export_file_missing", "导出文件不存在");
		try {
			const directory = await realpath(
				await existingArtifactDirectory(this.root, record.result.options.date, "sessions"),
			);
			const target = await realpath(join(directory, entry.name));
			const path = relative(directory, target);
			if (isAbsolute(path) || path.startsWith(`..${sep}`) || path === "..") throw new Error("invalid path");
			const data = await readFile(target);
			if (createHash("sha256").update(data).digest("hex") !== entry.hash)
				throw new ExportError(409, "export_file_changed", "导出文件已被修改，请重新导出后分享");
			return {
				name: entry.name,
				data,
				type: record.result.options.format === "html" ? "text/html; charset=utf-8" : "image/png",
			};
		} catch (cause) {
			if (cause instanceof ExportError) throw cause;
			throw new ExportError(404, "export_file_missing", "导出文件已移走或无法读取，请重新导出");
		}
	}
	close(): Promise<void> {
		this.closing = true;
		this.controller?.abort(new ExportError(503, "export_interrupted", "导出已随程序退出取消，请重新导出"));
		this.shutdown ??= (async () => {
			await Promise.allSettled(this.creating);
			await this.pumping;
			await this.renderer.close();
		})();
		return this.shutdown;
	}
}
