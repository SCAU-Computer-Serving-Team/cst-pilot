import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { openDeliveredRunbook, RunbookOpenError } from "../server/api/runbook.ts";
import { testRoot } from "./support.ts";

test("清单打开仅允许 outbox 内现存 txt，路径越界和非文本拒绝", async () => {
	const root = await mkdtemp(join(await testRoot(), "runbook-open-"));
	try {
		const outbox = join(root, "outbox");
		await mkdir(outbox);
		const file = join(outbox, "fixture.txt");
		await writeFile(file, "fixture command");
		const opened: string[] = [];
		const open = async (path: string) => {
			opened.push(path);
		};
		await openDeliveredRunbook(file, open, outbox);
		assert.equal(opened.length, 1);
		const outside = join(root, "outside.txt");
		await writeFile(outside, "outside");
		await assert.rejects(openDeliveredRunbook(outside, open, outbox));
		const executable = join(outbox, "fixture.cmd");
		await writeFile(executable, "fixture");
		await assert.rejects(openDeliveredRunbook(executable, open, outbox));
		await assert.rejects(
			openDeliveredRunbook(join(outbox, "missing.txt"), open, outbox),
			(error) => error instanceof RunbookOpenError && error.status === 404 && error.code === "runbook_missing",
		);
		await assert.rejects(
			openDeliveredRunbook(
				file,
				async () => {
					throw new Error("fixture spawn failure");
				},
				outbox,
			),
			(error) => error instanceof RunbookOpenError && error.status === 503 && error.code === "runbook_launch_failed",
		);
		assert.equal(opened.length, 1);
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});
