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
	configDir: string;
	sessionsDir: string;
	child: ChildProcess;
	close: () => Promise<void>;
};

/** 解析已构建的 Web 前端静态目录（供 `--static-dir` 使用，兼容默认构建与 `embed-web`）。 */
export function resolveWebStaticDir(): string {
	const published = join(repoRoot, "agent/home/extensions/web/static");
	if (existsSync(join(published, "index.html"))) {
		return published;
	}
	const viteClient = join(repoRoot, "src/web/frontend/build/client");
	if (existsSync(join(viteClient, "index.html"))) {
		return viteClient;
	}
	throw new Error(
		`未找到已构建的前端静态目录（已检查 ${published} 与 ${viteClient}）。请先运行 npm run build --prefix src/web/frontend`,
	);
}

/** 解析 Rust `cst-server` 可执行文件路径（优先使用 `CARGO_TARGET_DIR`，再回退至仓库 `target/debug`）。 */
export function resolveRustServerBin(): string {
	const exeName = process.platform === "win32" ? "cst-server.exe" : "cst-server";
	const candidates: string[] = [];
	if (process.env.CARGO_TARGET_DIR) {
		candidates.push(join(resolve(process.env.CARGO_TARGET_DIR), "debug", exeName));
	}
	candidates.push(join(repoRoot, "target", "debug", exeName));
	for (const candidate of candidates) {
		if (existsSync(candidate)) {
			return candidate;
		}
	}
	throw new Error(
		`未找到 cst-server 可执行文件（已检查：${candidates.join("、")}）。请先运行 cargo build -p cst-server --bin cst-server`,
	);
}

/** 按阶段 9.2 发行/隔离目录契约初始化临时 `kitRoot`（`data/config`、`data/sessions`、`app/*`）。 */
export async function prepareRustKitRoot(
	kitRootInput: string,
	modelBaseUrl?: string,
): Promise<{
	kitRoot: string;
	configDir: string;
	sessionsDir: string;
}> {
	const kitRoot = resolve(kitRootInput);
	const configDir = join(kitRoot, "data", "config");
	const sessionsDir = join(kitRoot, "data", "sessions");
	const promptsDir = join(kitRoot, "app", "prompts");
	const skillsDir = join(kitRoot, "app", "skills");
	const defaultsDir = join(kitRoot, "app", "defaults");
	const fontsDir = join(kitRoot, "app", "fonts");

	await mkdir(configDir, { recursive: true });
	await mkdir(sessionsDir, { recursive: true });
	await mkdir(promptsDir, { recursive: true });

	const appendTarget = join(promptsDir, "APPEND_SYSTEM.md");
	if (!existsSync(appendTarget)) {
		const appPrompt = join(repoRoot, "app/prompts/APPEND_SYSTEM.md");
		const fallbackPrompt = join(repoRoot, "agent/home/APPEND_SYSTEM.md");
		await cp(existsSync(appPrompt) ? appPrompt : fallbackPrompt, appendTarget);
	}
	if (!existsSync(skillsDir)) {
		const appSkills = join(repoRoot, "app/skills");
		const fallbackSkills = join(repoRoot, "agent/home/skills");
		await cp(existsSync(appSkills) ? appSkills : fallbackSkills, skillsDir, { recursive: true });
	}
	const appDefaults = join(repoRoot, "app/defaults");
	if (existsSync(appDefaults) && !existsSync(defaultsDir)) {
		await cp(appDefaults, defaultsDir, { recursive: true });
	}
	const sharedFontsSource = join(repoRoot, "assets/fonts");
	if (existsSync(sharedFontsSource) && !existsSync(fontsDir)) {
		await mkdir(dirname(fontsDir), { recursive: true });
		await symlink(sharedFontsSource, fontsDir, "junction");
	}
	if (!existsSync(join(configDir, "settings.json")) && !existsSync(join(configDir, "models.json"))) {
		await configureTestModel(configDir, modelBaseUrl);
	}

	return { kitRoot, configDir, sessionsDir };
}

/** 启动独立模式 Rust cst-server 子进程，供过渡期双入口浏览器 E2E 验收使用。 */
export async function startRustWebServer(options: {
	kitRoot: string;
	cwd?: string;
	port?: number;
	modelBaseUrl?: string;
	standaloneWeb?: boolean;
}): Promise<RustWebServerHandle> {
	const { kitRoot, configDir, sessionsDir } = await prepareRustKitRoot(options.kitRoot, options.modelBaseUrl);
	const staticDir = resolveWebStaticDir();
	const port = options.port ?? (await freePort());
	const bin = resolveRustServerBin();
	const args: string[] = [];
	if (options.standaloneWeb !== false) {
		args.push("--standalone-web");
	}
	args.push("--port", String(port), "--kit-root", kitRoot, "--cwd", options.cwd ?? kitRoot, "--static-dir", staticDir);

	const child = spawn(bin, args, {
		cwd: options.cwd ?? kitRoot,
		stdio: ["ignore", "pipe", "pipe"],
		windowsHide: true,
	});

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

	return { url, port, kitRoot, home: configDir, configDir, sessionsDir, child, close };
}
