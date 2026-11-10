import { type ChildProcess, execFile, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { cp, mkdir, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { freePort } from "./browser.ts";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../../../..");

/** 为只验证输入持久化的测试显式选择本机服务，避免依赖模型自动选择或宿主凭据。 */
export async function configureTestModel(home: string, baseUrl = "http://127.0.0.1:9/v1"): Promise<void> {
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "test-probe", defaultModel: "mock", defaultTools: ["read", "ls"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				"test-probe": {
					api: "openai-completions",
					baseUrl,
					apiKey: "test-only",
					models: [
						{
							id: "mock",
							name: "Mock Model",
							reasoning: false,
							input: ["text", "image"],
							contextWindow: 10000,
							maxTokens: 100,
						},
					],
				},
			},
		}),
	);
}

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

export type RustWebServerHandle = {
	url: string;
	port: number;
	kitRoot: string;
	home: string;
	child: ChildProcess;
	close: () => Promise<void>;
};

/** 启动独立模式 Rust cst-server 子进程，供过渡期双入口浏览器 E2E 验收使用。 */
export async function startRustWebServer(options: {
	kitRoot: string;
	cwd?: string;
	port?: number;
	modelBaseUrl?: string;
}): Promise<RustWebServerHandle> {
	const kitRoot = resolve(options.kitRoot);
	const home = join(kitRoot, "agent", "home");
	await mkdir(join(home, "sessions"), { recursive: true });

	const appendTarget = join(home, "APPEND_SYSTEM.md");
	if (!existsSync(appendTarget)) {
		await cp(join(repoRoot, "agent/home/APPEND_SYSTEM.md"), appendTarget);
	}
	const skillsTarget = join(home, "skills");
	if (!existsSync(skillsTarget)) {
		await cp(join(repoRoot, "agent/home/skills"), skillsTarget, { recursive: true });
	}
	const staticTarget = join(home, "extensions/web/static");
	if (!existsSync(staticTarget)) {
		await mkdir(dirname(staticTarget), { recursive: true });
		await symlink(join(repoRoot, "agent/home/extensions/web/static"), staticTarget, "junction");
	}
	if (!existsSync(join(home, "settings.json")) && !existsSync(join(home, "models.json"))) {
		await configureTestModel(home, options.modelBaseUrl);
	}

	const port = options.port ?? (await freePort());
	const bin = join(repoRoot, "target", "debug", process.platform === "win32" ? "cst-server.exe" : "cst-server");
	if (!existsSync(bin)) {
		throw new Error(`未找到 cst-server 可执行文件：${bin}。请先运行 cargo build -p cst-server --bin cst-server`);
	}

	const child = spawn(
		bin,
		["--standalone-web", "--port", String(port), "--kit-root", kitRoot, "--cwd", options.cwd ?? kitRoot],
		{
			cwd: options.cwd ?? kitRoot,
			stdio: ["ignore", "pipe", "pipe"],
			windowsHide: true,
		},
	);

	let logs = "";
	child.stdout?.on("data", (chunk: Buffer) => {
		logs = (logs + chunk.toString()).slice(-16_384);
	});
	child.stderr?.on("data", (chunk: Buffer) => {
		logs = (logs + chunk.toString()).slice(-16_384);
	});

	const url = `http://127.0.0.1:${port}`;
	const deadline = Date.now() + 15_000;
	while (Date.now() < deadline) {
		if (child.exitCode !== null) {
			throw new Error(`cst-server 启动后提前退出（code=${child.exitCode}）：\n${logs}`);
		}
		try {
			const res = await fetch(`${url}/api/health`, { signal: AbortSignal.timeout(500) });
			if (res.ok) break;
		} catch {
			/* 等待监听就绪。 */
		}
		await new Promise((r) => setTimeout(r, 40));
	}

	const close = async () => {
		if (child.exitCode !== null) return;
		try {
			await fetch(`${url}/api/lifecycle/exit`, {
				method: "POST",
				headers: {
					Origin: url,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
				},
				body: JSON.stringify({ confirm: "stop" }),
				signal: AbortSignal.timeout(3000),
			});
		} catch {
			/* 降级为进程终止。 */
		}
		const waitExit = new Promise<void>((resolveExit) => {
			if (child.exitCode !== null) {
				resolveExit();
				return;
			}
			const timer = setTimeout(resolveExit, 2000);
			child.once("exit", () => {
				clearTimeout(timer);
				resolveExit();
			});
		});
		await waitExit;
		if (child.exitCode === null && child.pid) {
			if (process.platform === "win32") {
				await new Promise<void>((resolveKill) => {
					execFile("taskkill", ["/pid", String(child.pid), "/t", "/f"], { windowsHide: true }, () =>
						resolveKill(),
					);
				});
			} else {
				child.kill("SIGKILL");
			}
		}
	};

	return { url, port, kitRoot, home, child, close };
}
