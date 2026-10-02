import { existsSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

/** Keep local test artifacts on the kit's temporary drive when available. */
export async function testRoot(): Promise<string> {
	const base =
		process.env.CST_WEB_TEST_TMPDIR ?? (existsSync("E:/") ? "E:/tmp" : existsSync("F:/") ? "F:/tmp" : tmpdir());
	const now = new Date();
	const date = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
	const root = join(base, date);
	await mkdir(root, { recursive: true });
	return root;
}
