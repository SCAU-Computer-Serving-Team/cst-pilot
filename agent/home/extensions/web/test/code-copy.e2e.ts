import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

const source = 'const 中文 = "<tag>&value";\n\tconsole.log(中文);\n\n';
const plain = "  第一行\n\t第二行\n";
const raw = '{\n  "路径": "C:/测试",\n  "ok": true\n}\n';

test("代码块复制：原文、独立反馈、键盘、失败重试、工具原文与浅深窄屏", { timeout: 40000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "code-copy-"));
	const now = Date.now();
	const sessions = [{ id: "copy", title: "代码复制", updatedAt: new Date(now).toISOString(), running: false }];
	const messages = [
		{ role: "user", content: "检查代码", timestamp: now },
		{
			role: "assistant",
			content: [
				{ type: "thinking", thinking: "保留原始输出。" },
				{ type: "toolCall", id: "raw", name: "read", arguments: { path: "fixture.json" } },
			],
			timestamp: now + 1,
			stopReason: "toolUse",
		},
		{
			role: "toolResult",
			toolCallId: "raw",
			toolName: "read",
			content: [{ type: "text", text: raw }],
			timestamp: now + 2,
		},
		{
			role: "assistant",
			content: [
				{
					type: "text",
					text: `行内代码 \`inline\` 不加按钮。\n\n\`\`\`typescript\n${source}\`\`\`\n\n\`\`\`\n${plain}\`\`\`\n\n    indented();\n\n\`\`\`text\n${"very_long_".repeat(60)}\n\`\`\``,
				},
			],
			timestamp: now + 3,
			stopReason: "stop",
		},
	];
	const clients = new Set<ServerResponse>();
	const port = await freePort();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions, stage: "copy-test" }),
		async (_req, res, path) => {
			if (!path.startsWith("/api/")) return false;
			if (path.endsWith("/events")) {
				res.writeHead(200, { "Content-Type": "text/event-stream" });
				res.write(": connected\n\n");
				clients.add(res);
				res.on("close", () => clients.delete(res));
				return true;
			}
			let data: unknown = {};
			if (path === "/api/sessions/copy")
				data = {
					id: "copy",
					messages,
					entries: messages.map((message, index) => ({ id: `entry-${index}`, message })),
					running: false,
					queue: { version: 0, items: [], paused: false },
					ui: [],
				};
			else if (path === "/api/sessions") data = { sessions };
			else if (path === "/api/models") data = { models: [], selected: null, enabled: null, thinkingLevels: [] };
			else if (path === "/api/settings") data = { theme: "light", providers: [], models: [], scopedModels: [] };
			else if (path === "/api/auth") data = { providers: [] };
			res.writeHead(200, { "Content-Type": "application/json", Connection: "close" });
			res.end(JSON.stringify(data));
			return true;
		},
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.navigate(`http://127.0.0.1:${port}/s/copy`);
		await browser.until("document.querySelectorAll('.markdown .code-copy-button').length===4", "四类代码块复制入口");
		assert.equal(await browser.evaluate("!!document.querySelector('.markdown pre .hljs-keyword')"), true);
		assert.equal(await browser.evaluate("!!document.querySelector('.markdown p .code-copy-button')"), false);
		await browser.evaluate(
			"window.__copied=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>window.__copied.push(text)}})",
		);
		await browser.click(".markdown .code-copy-button");
		await browser.until("!!document.querySelector('.markdown .code-copy-button[data-copied]')", "成功图标");
		assert.equal(await browser.evaluate("window.__copied[0]"), source, "高亮节点不影响原文、缩进、空行与尾部换行");
		await browser.evaluate("document.querySelectorAll('.markdown .code-copy-button')[1].focus()");
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyDown",
			key: "Enter",
			code: "Enter",
			windowsVirtualKeyCode: 13,
			text: "\r",
		});
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyUp",
			key: "Enter",
			code: "Enter",
			windowsVirtualKeyCode: 13,
		});
		await browser.until("window.__copied.length===2", "键盘复制");
		assert.equal(await browser.evaluate("window.__copied[1]"), plain);
		assert.equal(
			await browser.evaluate("document.querySelectorAll('.markdown .code-copy-button[data-copied]').length"),
			2,
		);
		await browser.until("!document.querySelector('.markdown .code-copy-button[data-copied]')", "成功反馈自动恢复");
		await browser.evaluate("navigator.clipboard.writeText=async()=>{throw Error('denied')}");
		await browser.click(".markdown .code-copy-button");
		await browser.until(
			"document.querySelector('.markdown .code-copy-feedback[data-error]')?.textContent.includes('复制失败')",
			"权限失败反馈",
		);
		assert.equal(
			await browser.evaluate("!!document.querySelector('.markdown .code-copy-button[data-copied]')"),
			false,
		);
		await browser.evaluate("navigator.clipboard.writeText=async text=>window.__copied.push(text)");
		await browser.click(".markdown .code-copy-button");
		await browser.until("!document.querySelector('.markdown .code-copy-feedback[data-error]')", "失败后可重试");
		assert.equal(await browser.evaluate("window.__copied.at(-1)"), source);
		await browser.click(".turn-process-head");
		await browser.click(".tool-group-heading");
		await browser.click(".tool-card-heading");
		await browser.click(".tool-card .code-copy-button");
		assert.equal(await browser.evaluate("window.__copied.at(-1)"), raw, "工具原文也可复制");
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('pre')].every(pre=>!!pre.closest('.code-block')?.querySelector('.code-copy-button'))",
			),
			true,
		);
		for (const theme of ["light", "dark"]) {
			await browser.evaluate(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
			await browser.evaluate(
				"document.querySelector('.markdown .code-block').scrollIntoView({block:'center',behavior:'instant'})",
			);
			await browser.screenshot(join(root, `desktop-${theme}.png`));
		}
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('[aria-label="收起侧栏"]');
		await browser.evaluate(
			"document.querySelector('.markdown .code-block').scrollIntoView({block:'center',behavior:'instant'})",
		);
		assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.markdown .code-copy-button')].every(button=>{const b=button.getBoundingClientRect(),p=button.closest('.code-block').querySelector('pre').getBoundingClientRect();return b.width>=32&&b.height>=32&&b.left>=p.left&&b.right<=p.right&&b.top>=p.top&&b.bottom<=p.bottom})",
			),
			true,
			"窄屏复制图标不越出代码块",
		);
		await browser.screenshot(join(root, "narrow-dark.png"));
		console.log("CODE_COPY_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const client of clients) client.end();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
