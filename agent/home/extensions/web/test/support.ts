import { existsSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

/** 为只验证输入持久化的测试显式选择本机服务，避免依赖模型自动选择或宿主凭据。 */
export async function configureTestModel(home: string): Promise<void> {
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
					baseUrl: "http://127.0.0.1:9/v1",
					apiKey: "test-only",
					models: [
						{ id: "mock", reasoning: false, input: ["text", "image"], contextWindow: 10000, maxTokens: 100 },
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
