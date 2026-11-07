import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { BrowserProbe } from "./browser.ts";
import { testRoot } from "./support.ts";

test("浏览器启动失败保留退出码与错误输出，并及时回收子进程", { timeout: 20_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "browser-startup-"));
	const previous = process.env.CHROME_PATH;
	process.env.CHROME_PATH = process.execPath;
	try {
		const started = Date.now();
		await assert.rejects(BrowserProbe.launch(root), /bad option|unknown option|unrecognized option/i);
		assert.ok(Date.now() - started < 5000, "已退出的浏览器无需等待完整启动期限");
		const diagnostic = JSON.parse(await readFile(join(root, "browser-startup.json"), "utf8"));
		assert.equal(diagnostic.executable, process.execPath);
		assert.notEqual(diagnostic.exitCode, null);
		assert.match(diagnostic.stderr, /headless/);
	} finally {
		if (previous === undefined) delete process.env.CHROME_PATH;
		else process.env.CHROME_PATH = previous;
	}
});

test("账号浏览器启动失败后关闭服务，测试进程自行退出", { timeout: 30_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "account-launch-failure-"));
	await mkdir(join(root, "agent"));
	const env = { ...process.env };
	delete env.NODE_TEST_CONTEXT;
	const child = spawn(process.execPath, ["--test", fileURLToPath(new URL("./account.e2e.ts", import.meta.url))], {
		env: {
			...env,
			CHROME_PATH: process.execPath,
			CST_WEB_TEST_TMPDIR: root,
			PI_CODING_AGENT_DIR: join(root, "agent"),
			PI_OFFLINE: "1",
		},
		stdio: ["ignore", "pipe", "pipe"],
	});
	let output = "";
	child.stdout.on("data", (data) => {
		output += data;
	});
	child.stderr.on("data", (data) => {
		output += data;
	});
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		const code = await Promise.race([
			new Promise<number | null>((resolve, reject) => {
				child.once("exit", resolve);
				child.once("error", reject);
			}),
			new Promise<never>((_, reject) => {
				timer = setTimeout(() => reject(new Error(`启动失败后进程未退出：${output}`)), 15_000);
			}),
		]);
		assert.equal(code, 1, output);
		assert.match(output, /bad option|unknown option|unrecognized option/i);
	} finally {
		clearTimeout(timer);
		if (child.exitCode === null && child.signalCode === null) {
			if (process.platform === "win32") {
				await new Promise<void>((resolve) => {
					execFile("taskkill", ["/pid", String(child.pid), "/t", "/f"], { windowsHide: true }, () => resolve());
				});
			} else child.kill("SIGKILL");
		}
	}
});
