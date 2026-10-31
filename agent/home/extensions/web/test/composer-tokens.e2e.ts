import assert from "node:assert/strict";
import { cp, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("输入标记：选择命令、继续输入、草稿恢复、移除和文件引用发送", { timeout: 60_000 }, async () => {
	const root = join(await testRoot(), "composer-tokens");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(join(home, "package-caret.txt"), "FILE_REFERENCE_FIXTURE");
	await cp(fileURLToPath(new URL("../../../skills/bitlocker/", import.meta.url)), join(home, "skills", "bitlocker"), {
		recursive: true,
	});
	await writeFile(join(home, "web-settings.json"), JSON.stringify({ theme: "dark" }));
	const received: string[] = [];
	const model = createServer(async (request, response) => {
		const chunks: Buffer[] = [];
		for await (const chunk of request) chunks.push(chunk);
		const input = JSON.parse(Buffer.concat(chunks).toString("utf8")) as {
			messages: { role: string; content: string | { type: string; text?: string }[] }[];
		};
		const content = input.messages.filter((message) => message.role === "user").at(-1)?.content ?? "";
		received.push(
			typeof content === "string"
				? content
				: content
						.filter((part) => part.type === "text")
						.map((part) => part.text ?? "")
						.join(""),
		);
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		response.write(
			`data: ${JSON.stringify({ id: "caret", choices: [{ index: 0, delta: { role: "assistant", content: "已收到文件引用" }, finish_reason: null }] })}\n\n`,
		);
		response.end(
			`data: ${JSON.stringify({ id: "caret", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	const modelPort = await freePort();
	await new Promise<void>((resolve) => model.listen(modelPort, "127.0.0.1", resolve));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "one", enabledModels: ["probe/one"], theme: "dark" }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: `http://127.0.0.1:${modelPort}/v1`,
					api: "openai-completions",
					apiKey: "test-only",
					models: [
						{
							id: "one",
							name: "输入验收模型",
							reasoning: false,
							input: ["text"],
							contextWindow: 10000,
							maxTokens: 100,
						},
					],
				},
			},
		}),
	);
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => ({ sessions: await pool.list(), stage: "test" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/`);
		await browser.until("document.querySelector('.model-name')?.textContent.includes('输入验收模型')", "输入框就绪");
		async function key(key: string) {
			const windowsVirtualKeyCode = key === "Backspace" ? 8 : key === "Enter" ? 13 : 0;
			await browser!.call("Input.dispatchKeyEvent", { type: "keyDown", key, code: key, windowsVirtualKeyCode });
			await browser!.call("Input.dispatchKeyEvent", { type: "keyUp", key, code: key, windowsVirtualKeyCode });
		}
		async function assertEditableAfterToken(label: string) {
			await browser!.call("Input.insertText", { text: "x" });
			const value = await browser!.evaluate<{ text: string; editable: boolean }>(
				"(()=>{const editor=document.querySelector('.composer-input'),node=window.getSelection()?.focusNode;return {text:editor.textContent,editable:node?.nodeType===Node.TEXT_NODE&&editor.contains(node)}})()",
			);
			assert.equal(value.editable, true, "TAG 后可以继续输入正文");
			assert.ok(value.text.endsWith("x"));
			await browser!.screenshot(join(root, `${label}.png`));
			await key("Backspace");
			return value;
		}
		await browser.fill(".composer-input", "/comp");
		await browser.until("!!document.querySelector('.command-suggestions .command-active')", "compact 补全");
		await key("Enter");
		await browser.until("!!document.querySelector('.composer-chip[data-token=\"/compact\"]')", "compact TAG");
		const command = await assertEditableAfterToken("command-input");
		await browser.call("Input.insertText", { text: "保留关键结论" });
		assert.equal(
			await browser.evaluate("Object.values(sessionStorage).some(value=>value==='/compact 保留关键结论')"),
			true,
			`视觉间距不进入命令草稿正文：${JSON.stringify(await browser.evaluate("({drafts:Object.fromEntries(Object.entries(sessionStorage)),html:document.querySelector('.composer-input').innerHTML})"))}`,
		);
		await browser.navigate(`${origin}/`);
		await browser.until("document.querySelector('.composer-chip')?.dataset.token==='/compact'", "恢复命令 TAG 草稿");
		await browser.click(".chip-remove");
		await browser.until("!document.querySelector('.composer-chip')", "移除命令 TAG");
		assert.equal(
			await browser.evaluate("document.querySelector('.composer-input').textContent"),
			"保留关键结论",
			"移除 TAG 不残留视觉间距",
		);
		await browser.fill(".composer-input", "@package");
		await browser.until("!!document.querySelector('.file-suggestions .command-active')", "文件补全");
		await key("Enter");
		await browser.until("!!document.querySelector('.composer-chip[data-token^=\"@\"]')", "文件引用 TAG");
		const token = await browser.evaluate<string>("document.querySelector('.composer-chip').dataset.token");
		const file = await assertEditableAfterToken("file-input");
		await browser.call("Input.insertText", { text: "查看" });
		await browser.click('[aria-label="发送消息"]');
		await browser.until(
			"location.pathname.startsWith('/s/')&&document.querySelector('.conversation')?.textContent.includes('已收到文件引用')",
			"文件引用真实发送",
		);
		assert.equal(
			await browser.evaluate(
				"(()=>{const rect=s=>document.querySelector(s).getBoundingClientRect();const context=rect('.context-meter'),model=rect('.composer-model'),send=rect('.composer-send');return Math.abs(model.left-context.right-8)<1&&Math.abs(send.left-model.right-16)<1&&Math.abs(model.top+model.height/2-(send.top+send.height/2))<1})()",
			),
			true,
			"上下文距模型 8px，组合右侧留白与操作栏间距合计 16px，控件垂直居中",
		);
		assert.ok(
			received.some((text) => text === `${token} 查看`),
			`模型收到规范文件引用和正文，视觉间距不进入请求：${JSON.stringify({ token, received })}`,
		);
		const hoverAction = async (selector: string, label: string) => {
			const point = await browser!.evaluate<{ x: number; y: number }>(
				`(()=>{const r=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();return{x:r.left+r.width/2,y:r.top+r.height/2}})()`,
			);
			await browser!.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...point });
			await browser!.until(
				`document.querySelector('[role=tooltip]')?.textContent.includes(${JSON.stringify(label)})`,
				`${label} 提示`,
			);
		};
		await hoverAction('.answer-actions [aria-label="复制回答"]', "复制");
		await hoverAction('.answer-actions [aria-label="从此处分支新会话"]', "从此处分支新会话");
		await browser.fill(".composer-input", "/skill:bit");
		await browser.until("!!document.querySelector('.command-suggestions .command-active')", "BitLocker 技能补全");
		await key("Enter");
		await browser.until(
			"!!document.querySelector('.composer-chip[data-token=\"/skill:bitlocker\"]')",
			"BitLocker 技能 TAG",
		);
		await browser.call("Input.insertText", { text: "查询状态" });
		await browser.click('[aria-label="发送消息"]');
		await browser.until(
			"document.querySelector('.conversation')?.textContent.includes('查询状态')",
			"BitLocker 技能发送",
		);
		for (
			let attempt = 0;
			attempt < 100 && !received.some((text) => text.includes('<skill name="bitlocker"'));
			attempt++
		) {
			await new Promise((resolve) => setTimeout(resolve, 30));
		}
		assert.ok(
			received.some((text) => text.includes('<skill name="bitlocker"') && text.includes("查询状态")),
			"显式技能由 Pi 展开后发给本机模型替身",
		);
		await writeFile(
			join(root, "report.json"),
			JSON.stringify(
				{ command, file, draftRestored: true, removedWithoutGap: true, fileReferenceSent: true },
				null,
				2,
			),
		);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		model.closeAllConnections();
		await new Promise<void>((resolve) => model.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
