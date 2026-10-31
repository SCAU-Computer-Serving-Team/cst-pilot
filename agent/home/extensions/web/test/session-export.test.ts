import assert from "node:assert/strict";
import { mkdir, mkdtemp, readdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { artifactDate, artifactName, ensureArtifactDirectory } from "../../diagnostics/artifact-paths.ts";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";
import { SessionExportStore } from "../server/exports/store.ts";
import type { ConversationEntry } from "../shared/conversation.ts";
import type { ExportSnapshot, SessionExport } from "../shared/exports.ts";
import { testRoot } from "./support.ts";

const entries: ConversationEntry[] = [
	{ id: "u1", message: { role: "user", content: "第一轮", timestamp: 1 } },
	{
		id: "a1",
		message: {
			role: "assistant",
			content: [
				{ type: "thinking", thinking: "隐藏思考" },
				{ type: "toolCall", id: "t1", name: "read", arguments: {} },
			],
			timestamp: 2,
		},
	},
	{ id: "t1", message: { role: "toolResult", toolCallId: "t1", toolName: "read", content: "工具输出", timestamp: 3 } },
	{ id: "a2", message: { role: "assistant", content: [{ type: "text", text: "第一轮回答" }], timestamp: 4 } },
	{ id: "u2", message: { role: "user", content: "第二轮", timestamp: 5 } },
	{ id: "a3", message: { role: "assistant", content: "第二轮回答", timestamp: 6 } },
];
const options = exportOptions({ format: "html", theme: "dark" });
const day = artifactDate();
const snapshot = (): ExportSnapshot => exportSnapshot("fixture", "测试标题", entries, options);
async function completed(store: SessionExportStore, id: string): Promise<SessionExport> {
	for (let attempt = 0; attempt < 200; attempt++) {
		const result = await store.get(id);
		if (result.status === "completed" || result.status === "failed") return result;
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	throw new Error("测试导出未完成");
}

test("导出时间由快照时刻决定，跨天重新生成使用新的本机日期", (context) => {
	const before = new Date(2026, 9, 31, 23, 59, 59);
	context.mock.timers.enable({ apis: ["Date"], now: before });
	const preferences = exportOptions({ format: "html" });
	assert.equal("date" in preferences, false);
	const first = exportSnapshot("fixture", "跨天", entries, preferences);
	assert.equal(first.options.date, "2026-10-31");
	assert.equal(first.capturedAt, before.toISOString());
	const after = new Date(2026, 10, 1, 0, 0, 1);
	context.mock.timers.setTime(after.getTime());
	const second = exportSnapshot("fixture", "跨天", entries, preferences);
	assert.equal(second.options.date, "2026-11-01");
	assert.equal(second.capturedAt, after.toISOString());
});

test("导出选项与文件名校验，当前分支和过程包含规则不修改原消息", () => {
	assert.equal(artifactDate("2024-02-29"), "2024-02-29");
	for (const day of ["2026-02-29", "2026-13-01", "2026-02-30", "../escape", "2026-1-1", "0000-01-01"])
		assert.throws(() => artifactDate(day));
	assert.equal(artifactName(' title<>:"/\\|?*. '), "title");
	for (const input of [
		{ format: "pdf" },
		{ theme: "system" },
		{ includeTools: "yes" },
		{ date: "2026-10-29" },
		{ turnIds: ["u1"] },
		{ extra: true },
	])
		assert.throws(() => exportOptions(input));
	const original = JSON.stringify(entries);
	const result = snapshot();
	assert.equal(
		result.entries.some(({ message }) => message.role === "toolResult"),
		false,
	);
	assert.equal(JSON.stringify(result).includes("隐藏思考"), false);
	assert.equal(JSON.stringify(result).includes("toolCall"), false);
	assert.deepEqual(
		result.entries.map((entry) => entry.id),
		["u1", "a2", "u2", "a3"],
	);
	const full = exportSnapshot("fixture", "全量", entries, { ...options, includeThinking: true, includeTools: true });
	assert.equal(full.entries.length, entries.length);
	assert.equal(JSON.stringify(entries), original);
	const secret = Array(8).fill("123456").join("-");
	const redacted = exportSnapshot(
		"fixture",
		secret,
		[{ id: "secret", message: { role: "user", content: secret, timestamp: 1 } }],
		options,
	);
	assert.equal(JSON.stringify(redacted).includes(secret), false);
});

test("产物目录拒绝符号链接跨界，日期始终为第二级", async () => {
	const root = await mkdtemp(join(await testRoot(), "artifacts-path-"));
	try {
		const artifacts = join(root, "artifacts"),
			outside = join(root, "outside");
		await mkdir(outside);
		assert.equal(await ensureArtifactDirectory(artifacts, day, "sessions"), join(artifacts, day, "sessions"));
		await rm(join(artifacts, day, "sessions"), { recursive: true });
		await symlink(outside, join(artifacts, day, "sessions"), "junction");
		await assert.rejects(ensureArtifactDirectory(artifacts, day, "sessions"));
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});

test("作业文件登记可重启恢复，下载仅访问已登记文件并检测修改", async () => {
	const root = await mkdtemp(join(await testRoot(), "export-store-"));
	const artifactRoot = join(root, "artifacts"),
		metadata = join(root, "home", "session-exports");
	const renderer = async (
		_snapshot: ExportSnapshot,
		_url: string,
		emit: (extension: "html" | "png", bytes: Buffer) => Promise<void>,
	) => {
		await emit("html", Buffer.from("<!DOCTYPE html><p>fixture</p>"));
	};
	const store = new SessionExportStore(artifactRoot, metadata, "http://127.0.0.1:9999", renderer);
	try {
		const created = await store.create(snapshot());
		const result = await completed(store, created.id);
		assert.equal(result.status, "completed");
		assert.equal(result.directory, join(artifactRoot, day, "sessions"));
		assert.equal((await readdir(result.directory)).length, 1);
		const restored = new SessionExportStore(artifactRoot, metadata, "http://127.0.0.1:9999", renderer);
		assert.deepEqual(await restored.get(created.id), result);
		assert.equal((await restored.list("fixture")).length, 1);
		assert.equal((await restored.file(created.id, 0)).data.toString(), "<!DOCTYPE html><p>fixture</p>");
		await assert.rejects(restored.file(created.id, -1));
		await assert.rejects(restored.file(created.id, 1));
		await assert.rejects(restored.get("../outside"));
		assert.throws(() => restored.snapshot(created.id, "invalid"));
		await writeFile(join(result.directory, result.files[0].name), "changed");
		await assert.rejects(restored.file(created.id, 0), /修改/);
		await restored.close();
	} finally {
		await store.close();
		await rm(root, { recursive: true, force: true });
	}
});

test("渲染失败与存储写入失败不留下半成品，任务队列按顺序执行", async () => {
	const root = await mkdtemp(join(await testRoot(), "export-failure-"));
	const active: string[] = [];
	let concurrent = 0;
	const store: SessionExportStore = new SessionExportStore(
		join(root, "artifacts"),
		join(root, "metadata"),
		"http://127.0.0.1:9999",
		async (value, url, emit) => {
			assert.equal(concurrent++, 0);
			const [id, token] = new URL(url).hash.slice(1).split(":");
			assert.equal(store.snapshot(id, token).title, value.title);
			assert.throws(() => store.snapshot(id, "wrong-token"));
			await emit("html", Buffer.from("<!DOCTYPE html>fixture"));
			concurrent--;
			active.push(value.title);
			if (value.title === "失败") throw new Error("fixture render failure");
		},
	);
	try {
		const first = await store.create({ ...snapshot(), title: "失败" });
		const second = await store.create({ ...snapshot(), title: "成功" });
		assert.equal((await completed(store, first.id)).status, "failed");
		assert.equal((await completed(store, second.id)).status, "completed");
		assert.deepEqual(active, ["失败", "成功"]);
		assert.equal((await readdir(join(root, "artifacts", day, "sessions"))).length, 1);
		await store.close();
		assert.deepEqual(await readdir(join(root, "metadata", "work")), []);
		await writeFile(join(root, "readonly"), "not a directory");
		const blocked = new SessionExportStore(
			join(root, "readonly"),
			join(root, "blocked-metadata"),
			"http://127.0.0.1:9999",
			async (_value, _url, emit) => {
				await emit("html", Buffer.from("<!DOCTYPE html>fixture"));
			},
		);
		const failed = await blocked.create(snapshot());
		assert.equal((await completed(blocked, failed.id)).status, "failed");
		await blocked.close();
		assert.equal(await readFile(join(root, "readonly"), "utf8"), "not a directory");
	} finally {
		await store.close();
		await rm(root, { recursive: true, force: true });
	}
});
