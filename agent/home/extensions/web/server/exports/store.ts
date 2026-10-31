import { createHash, randomUUID, timingSafeEqual } from "node:crypto";
import { constants } from "node:fs";
import { copyFile, mkdir, readdir, readFile, realpath, rename, rm, writeFile } from "node:fs/promises";
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
type Work = { record: ExportRecord; snapshot: ExportSnapshot; token: string };
const VALID_ID = /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/;

/** 文件登记与作业队列归后端所有；sessions 目录只存最终 PNG / HTML。 */
export class SessionExportStore {
	private readonly root: string;
	private readonly metadata: string;
	private readonly origin: string;
	private readonly renderer: ExportRenderer;
	private readonly queue: Work[] = [];
	private active?: Work;
	private controller?: AbortController;
	private pumping?: Promise<void>;
	private closing = false;
	private reservations = 0;
	constructor(root: string, metadata: string, origin: string, renderer: ExportRenderer) {
		this.root = root;
		this.metadata = metadata;
		this.origin = origin;
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
	async create(source: ExportSnapshot): Promise<SessionExport> {
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
			const work: Work = { record: { result, stored: [] }, snapshot, token: randomUUID() };
			await this.save(work.record);
			if (this.closing) throw new ExportError(503, "export_stopping", "程序正在退出，请稍后导出");
			this.queue.push(work);
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
		}
	}
	private async run(work: Work): Promise<void> {
		const { result } = work.record;
		const published: string[] = [];
		const staging = join(this.metadata, "work", result.id);
		this.controller = new AbortController();
		const timeout = setTimeout(() => this.controller?.abort(), 120_000);
		try {
			if (this.closing) throw new ExportError(503, "export_interrupted", "导出已随程序退出取消，请重新导出");
			result.status = "rendering";
			await this.save(work.record);
			await mkdir(staging, { recursive: true });
			const files: { file: string; name: string; hash: string; bytes: number }[] = [];
			let bytes = 0;
			await this.renderer(
				work.snapshot,
				`${this.origin}/_export#${result.id}:${work.token}`,
				async (extension, data) => {
					this.controller?.signal.throwIfAborted();
					bytes += data.length;
					if (
						extension !== result.options.format ||
						files.length >= 64 ||
						bytes > 200 * 1024 * 1024 ||
						!data.length
					)
						throw new ExportError(413, "export_too_large", "产物超过大小限制，请缩短当前分支后重试");
					const suffix = extension === "png" ? `-${String(files.length + 1).padStart(2, "0")}` : "";
					const name = `${artifactName(result.title)}-${result.id}${suffix}.${extension}`;
					const file = join(staging, name);
					await writeFile(file, data, { flag: "wx" });
					files.push({ file, name, hash: createHash("sha256").update(data).digest("hex"), bytes: data.length });
				},
				this.controller.signal,
			);
			this.controller.signal.throwIfAborted();
			if (!files.length) throw new Error("导出未生成文件");
			const directory = await ensureArtifactDirectory(this.root, result.options.date, "sessions");
			for (const file of files) {
				this.controller.signal.throwIfAborted();
				const target = join(directory, file.name);
				await copyFile(file.file, target, constants.COPYFILE_EXCL);
				published.push(target);
			}
			this.controller.signal.throwIfAborted();
			work.record.stored = files.map(({ name, hash }) => ({ name, hash }));
			result.files = files.map(({ name, bytes }, index) => ({
				name,
				bytes,
				url: `/api/exports/${result.id}/files/${index}`,
			}));
			result.status = "completed";
			await this.save(work.record);
		} catch (cause) {
			await Promise.all(published.map((file) => rm(file, { force: true }).catch(() => undefined)));
			result.status = "failed";
			result.files = [];
			work.record.stored = [];
			const diskCode = cause && typeof cause === "object" && "code" in cause ? cause.code : "";
			result.error =
				cause instanceof ExportError
					? { code: cause.code, message: cause.message }
					: {
							code: diskCode === "ENOSPC" ? "storage_full" : "export_failed",
							message: this.controller.signal.aborted
								? "导出超时或已取消，请重试或缩短当前分支"
								: diskCode === "ENOSPC"
									? "工具包存储空间不足，请清理后重试"
									: "导出未完成，请检查浏览器和工具包写入权限后重试",
						};
			await this.save(work.record).catch(() => undefined);
		} finally {
			clearTimeout(timeout);
			this.controller = undefined;
			await rm(staging, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 }).catch(() => undefined);
		}
	}
	private async read(id: string): Promise<ExportRecord> {
		if (!VALID_ID.test(id)) throw new ExportError(404, "export_not_found", "导出记录不存在");
		try {
			const record = JSON.parse(await readFile(join(this.metadata, `${id}.json`), "utf8")) as ExportRecord;
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
			if (
				(record.result.status === "queued" || record.result.status === "rendering") &&
				this.active?.record.result.id !== id &&
				!this.queue.some((item) => item.record.result.id === id)
			) {
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
	snapshot(id: string, token: string): ExportSnapshot {
		const work = this.active;
		if (
			!work ||
			work.record.result.id !== id ||
			Buffer.byteLength(token) !== Buffer.byteLength(work.token) ||
			!timingSafeEqual(Buffer.from(token), Buffer.from(work.token))
		)
			throw new ExportError(404, "export_not_found", "导出预览已失效");
		return work.snapshot;
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
	async close(): Promise<void> {
		this.closing = true;
		this.controller?.abort();
		await this.pumping;
	}
}
