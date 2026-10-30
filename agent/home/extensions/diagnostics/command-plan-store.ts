import { randomUUID } from "node:crypto";
import { mkdir, readdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { artifactDate, ensureArtifactDirectory } from "./artifact-paths.ts";
import {
	buildBody,
	LEVELS,
	type Level,
	localStamp,
	type RunbookItem,
	type RunbookParams,
	serialize,
	validateRunbook,
	withDirectoryLock,
} from "./runbook-files.ts";

type PlanInput = Omit<RunbookParams, "sequence"> & { runbookId?: string; baseVersion?: number };
type Plan = {
	runbookId: string;
	version: number;
	day: string;
	createdAt: string;
	timeText: string;
	title: string;
	level: Level;
	levelLabel: string;
	commands: RunbookItem[];
};

const validId = (value: string) => /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/.test(value);

/** 方案版本是事实来源；文本文件按指定版本导出，重试沿用同一路径。 */
export class CommandPlanStore {
	private readonly plans: string;
	private readonly artifacts: string;
	private readonly legacyPlans?: string;
	constructor(
		artifacts: string,
		plans = join(artifacts, "..", "agent", "home", "command-plans"),
		legacyPlans?: string,
	) {
		this.artifacts = artifacts;
		this.plans = plans;
		this.legacyPlans = legacyPlans;
	}

	private async locked<T>(task: () => Promise<T>): Promise<T> {
		return serialize(async () => {
			await mkdir(this.plans, { recursive: true });
			return withDirectoryLock(this.plans, task);
		});
	}

	private async latest(runbookId: string): Promise<Plan> {
		if (!validId(runbookId)) throw new Error("清单 ID 无效");
		let dir = join(this.plans, runbookId);
		const versions = await readdir(dir).catch(async (cause: NodeJS.ErrnoException) => {
			if (cause.code === "ENOENT" && this.legacyPlans) {
				dir = join(this.legacyPlans, runbookId);
				return readdir(dir).catch(() => {
					throw new Error("命令方案不存在，请先调用 command_plan");
				});
			}
			if (cause.code === "ENOENT") throw new Error("命令方案不存在，请先调用 command_plan");
			throw cause;
		});
		const version = Math.max(
			0,
			...versions.filter((name) => /^v[1-9]\d*\.json$/.test(name)).map((name) => Number(name.slice(1, -5))),
		);
		if (!version) throw new Error("命令方案尚未保存，请重新生成");
		const raw = JSON.parse(await readFile(join(dir, `v${version}.json`), "utf8")) as Plan;
		if (
			raw.runbookId !== runbookId ||
			raw.version !== version ||
			!/^\d{4}-\d{2}-\d{2}$/.test(raw.day) ||
			!/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/.test(raw.timeText) ||
			typeof raw.createdAt !== "string" ||
			!Number.isFinite(Date.parse(raw.createdAt))
		)
			throw new Error("命令方案记录损坏，请重新生成");
		const normalized = validateRunbook({ title: raw.title, level: raw.level, items: raw.commands });
		return {
			...raw,
			title: normalized.title,
			level: normalized.level,
			commands: normalized.items,
			levelLabel: LEVELS[normalized.level],
		};
	}

	private async atomic(file: string, body: string): Promise<void> {
		const temporary = `${file}.${randomUUID()}.tmp`;
		try {
			await writeFile(temporary, body, { flag: "wx", encoding: "utf8" });
			await rename(temporary, file);
		} finally {
			await rm(temporary, { force: true });
		}
	}

	async plan(input: PlanInput): Promise<Plan> {
		const { title, level, items } = validateRunbook(input);
		if ((input.runbookId === undefined) !== (input.baseVersion === undefined))
			throw new Error("修订方案须同时提供 runbookId 与 baseVersion");
		return this.locked(async () => {
			const previous = input.runbookId === undefined ? undefined : await this.latest(input.runbookId);
			if (previous && previous.version !== input.baseVersion)
				throw new Error(`方案已有新版本 ${previous.version}，请核对最新命令后再修订`);
			const now = new Date();
			const day = artifactDate(input.date ?? previous?.day, now);
			if (previous && day !== previous.day) throw new Error("已有方案的归档日期不能修改");
			const plan: Plan = {
				runbookId: previous?.runbookId ?? randomUUID(),
				version: (previous?.version ?? 0) + 1,
				day,
				createdAt: now.toISOString(),
				timeText: localStamp(now).text,
				title,
				level,
				levelLabel: LEVELS[level],
				commands: items,
			};
			const dir = join(this.plans, plan.runbookId);
			await mkdir(dir, { recursive: true });
			await this.atomic(join(dir, `v${plan.version}.json`), JSON.stringify(plan));
			return plan;
		});
	}

	async deliver(runbookId: string, version: number) {
		if (!Number.isSafeInteger(version) || version < 1) throw new Error("version 必须是正整数");
		return this.locked(async () => {
			const plan = await this.latest(runbookId);
			if (plan.version !== version) throw new Error(`方案已有新版本 ${plan.version}，请核对后交付最新版本`);
			const base = await ensureArtifactDirectory(this.artifacts, plan.day, "runbook");
			const dir = join(base, plan.runbookId);
			const name = `v${plan.version}-${plan.levelLabel}-${plan.title}.txt`;
			const file = join(dir, name);
			const body = buildBody(plan.title, plan.level, plan.commands, plan.timeText);
			const existing = await readFile(file, "utf8").catch((cause: NodeJS.ErrnoException) => {
				if (cause.code === "ENOENT") return undefined;
				throw cause;
			});
			if (existing !== undefined && existing !== body)
				throw new Error("已交付文件被修改，请核对后修订方案，避免覆盖现场内容");
			if (existing === undefined) {
				await mkdir(dir, { recursive: true });
				await this.atomic(file, body);
			}
			return {
				...plan,
				items: plan.commands.length,
				action: "delivered",
				file,
				dir,
				name,
				bytes: Buffer.byteLength(body, "utf8"),
				encoding: "utf-8-bom",
			};
		});
	}
}
