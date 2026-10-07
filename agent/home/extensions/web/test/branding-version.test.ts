import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import { readKitVersion } from "../../branding/version.ts";
import { testRoot } from "./support.ts";

test("页眉读取发行VERSION，开发/损坏版本不显示虚假版本或控制字符", async () => {
	const dir = await mkdtemp(join(await testRoot(), "version-")),
		file = pathToFileURL(join(dir, "VERSION"));
	try {
		assert.equal(readKitVersion(file), "dev");
		await writeFile(file, "cst-pilot prev0.5\npi 0.85.1\n");
		assert.equal(readKitVersion(file), "prev0.5");
		await writeFile(file, "cst-pilot invalid\x1b[2J\n");
		assert.equal(readKitVersion(file), "dev");
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});
