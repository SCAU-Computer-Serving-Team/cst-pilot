import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import type { ContentPart, Message } from "../shared/conversation.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

type Input = { id: string; text: string; images: { mimeType: string; data: string }[] };
type Receipt = {
	id: string;
	text: string;
	delivery: string;
	status: string;
	sequence: number;
	images: [];
	uncertain?: boolean;
};
type Session = { id: string; messages: Message[]; receipts: Receipt[] };

test(
	"乐观发送：立即反馈、失败保留新草稿与图片、幂等重试、刷新核对、首页接管及一次入场",
	{ timeout: 90_000 },
	async () => {
		const root = await mkdtemp(join(await testRoot(), "optimistic-send-"));
		const sessions = new Map<string, Session>([["s1", { id: "s1", messages: [], receipts: [] }]]);
		const clients = new Map<string, Set<ServerResponse>>();
		const creations = new Map<string, string>();
		const writes: { sessionId: string; input: Input }[] = [];
		let mode: "hold" | "reject" | "accept" | "unknown" | "lost" = "hold";
		let release: (() => void) | undefined;
		let holdCreation = false;
		let releaseCreation: (() => void) | undefined;
		let creationWrites = 0;
		let stamp = Date.now();
		const json = (response: ServerResponse, value: unknown, status = 200) => {
			response.writeHead(status, { "Content-Type": "application/json" });
			response.end(JSON.stringify(value));
		};
		const publish = (sessionId: string, name: string, value: unknown) => {
			for (const response of clients.get(sessionId) ?? [])
				response.write(`event: ${name}\ndata: ${JSON.stringify(value)}\n\n`);
		};
		const detail = (session: Session) => ({
			...session,
			entries: session.messages.map((message, i) => ({ id: `entry-${i}`, message })),
			running: false,
			queue: { version: session.receipts.length, paused: false, items: session.receipts },
			ui: [],
		});
		const port = await freePort();
		const origin = `http://127.0.0.1:${port}`;
		const model = {
			provider: "probe",
			id: "mock",
			name: "测试模型",
			reasoning: false,
			credentialReady: true,
			catalogPresent: true,
			thinkingLevel: "off",
		};
		const rows = () =>
			[...sessions.values()].map((session) => ({
				id: session.id,
				title: session.messages.length ? "测试会话 · 已发送" : "测试会话",
				running: false,
				updatedAt: new Date().toISOString(),
			}));
		const server = createWebServer(
			fileURLToPath(new URL("../static/", import.meta.url)),
			port,
			() => ({ sessions: rows(), stage: "test", commands: [] }),
			async (request, response, path) => {
				if (!path.startsWith("/api/")) return false;
				if (path.endsWith("/events")) {
					const id = /^\/api\/sessions\/([^/]+)\/events$/.exec(path)?.[1] ?? "global";
					response.writeHead(200, { "Content-Type": "text/event-stream" });
					response.write(": connected\n\n");
					const set = clients.get(id) ?? new Set();
					set.add(response);
					clients.set(id, set);
					response.on("close", () => set.delete(response));
					return true;
				}
				if (path === "/api/sessions" && request.method === "POST") {
					creationWrites++;
					const key = String(request.headers["idempotency-key"]);
					if (holdCreation)
						await new Promise<void>((resolve) => {
							releaseCreation = resolve;
						});
					const id = creations.get(key) ?? `s${sessions.size + 1}`;
					creations.set(key, id);
					if (!sessions.has(id)) sessions.set(id, { id, messages: [], receipts: [] });
					json(response, { id }, 201);
					return true;
				}
				const creation = /^\/api\/sessions\/creation\/([^/]+)$/.exec(path);
				if (creation) {
					const id = creations.get(creation[1]);
					json(response, id ? { status: "done", sessionId: id } : { status: "missing" });
					return true;
				}
				const match = /^\/api\/sessions\/([^/]+)(?:\/(.*))?$/.exec(path);
				if (match) {
					const session = sessions.get(match[1]);
					if (!session) {
						json(response, {}, 404);
						return true;
					}
					const suffix = match[2] ?? "";
					if (!suffix) {
						json(response, detail(session));
						return true;
					}
					if (suffix.startsWith("messages/")) {
						json(response, { item: session.receipts.find((item) => item.id === suffix.slice(9)) ?? null });
						return true;
					}
					if (suffix === "messages" && request.method === "POST") {
						const chunks: Buffer[] = [];
						for await (const chunk of request) chunks.push(chunk);
						const input: Input = JSON.parse(Buffer.concat(chunks).toString());
						writes.push({ sessionId: session.id, input });
						if (mode === "hold")
							await new Promise<void>((resolve) => {
								release = resolve;
							});
						if (mode === "reject") {
							json(
								response,
								{ error: { code: "credentials_required", message: "所选模型缺少凭据，请配置后重试。" } },
								401,
							);
							return true;
						}
						if (mode === "unknown") {
							response.destroy();
							return true;
						}
						let item = session.receipts.find((item) => item.id === input.id);
						if (!item) {
							item = {
								id: input.id,
								text: input.text,
								status: "delivered",
								delivery: "direct",
								sequence: session.receipts.length + 1,
								images: [],
							};
							session.receipts.push(item);
							const content: ContentPart[] = [
								{ type: "text", text: input.text },
								...input.images.map((image) => ({ ...image, type: "image" as const })),
							];
							const message: Message = { role: "user", content, timestamp: ++stamp, clientMessageId: input.id };
							session.messages.push(message);
							publish(session.id, "session", { type: "message_end", message });
							publish(session.id, "queue", {
								version: session.receipts.length,
								paused: false,
								items: session.receipts,
							});
						}
						if (mode === "lost") response.destroy();
						else json(response, { item }, 202);
						return true;
					}
					json(response, {});
					return true;
				}
				if (path === "/api/models")
					json(response, {
						models: [model],
						catalog: [model],
						selected: model,
						defaultModel: model,
						enabled: null,
						unavailableEnabled: [],
						thinkingLevels: ["off"],
					});
				else if (path === "/api/sessions") json(response, { sessions: rows() });
				else if (path === "/api/account")
					json(response, {
						providerId: "cstoa",
						signedIn: false,
						requiresLogin: false,
						profile: { supported: false, studentId: null, name: null },
					});
				else if (path === "/api/auth") json(response, { providers: [] });
				else if (path === "/api/settings") json(response, { theme: "light", providers: [], models: [model] });
				else if (path === "/api/files") json(response, { files: [] });
				else json(response, {});
				return true;
			},
		);
		await listenWebServer(server, port);
		let browser: BrowserProbe | undefined;
		const clickAction = async (text: string) =>
			browser?.evaluate(
				`([...document.querySelectorAll('.outgoing-actions button')].find(e=>e.textContent.trim()===${JSON.stringify(text)})).click()`,
			);
		try {
			browser = await BrowserProbe.launch(root);
			await browser.navigate(`${origin}/s/s1`);
			await browser.until(
				"!!document.querySelector('.chat-loaded') && !!document.querySelector('.model-name')",
				"模型与会话就绪",
			);
			await browser.fill(".composer-input", "第一条待确认消息");
			await browser.click('[aria-label="发送消息"]');
			await browser.until(
				"document.querySelector('.composer-input')?.textContent==='' && document.querySelector('.user-bubble')?.textContent.includes('第一条待确认消息')",
				"后端尚未回应时清空输入并立即展示",
			);
			assert.equal(sessions.get("s1")?.messages.length, 0);
			await browser.evaluate(
				"window.__bubble=document.querySelector('.user-bubble');window.__animationStarts=0;document.addEventListener('animationstart',e=>{if(e.animationName==='message-rise')window.__animationStarts++});",
			);
			const timing = await browser.evaluate<{ duration: number; easing: string; distance: string }>(
				`(()=>{const a=document.querySelector('.user-bubble').getAnimations()[0];return {duration:a?.effect.getTiming().duration??250,easing:getComputedStyle(document.querySelector('.user-bubble')).animationTimingFunction,distance:getComputedStyle(document.documentElement).getPropertyValue('--distance-base').trim()}})()`,
			);
			assert.equal(timing.duration, 250);
			assert.equal(timing.distance, "8px");
			assert.ok(timing.easing.includes("0.22"));
			await browser.fill(".composer-input", "正在写的下一条草稿");
			mode = "reject";
			release?.();
			await browser.until("!!document.querySelector('.outgoing-status[data-state=failed]')", "明确拒绝进入失败态");
			assert.equal(
				await browser.evaluate("document.querySelector('.composer-input').textContent"),
				"正在写的下一条草稿",
			);
			await browser.evaluate("window.confirm=()=>true");
			await clickAction("编辑");
			await browser.until(
				"document.querySelector('.composer-input')?.textContent==='第一条待确认消息' && !!document.querySelector('.composer-saved-draft')",
				"编辑失败消息并保留新草稿",
			);
			await browser.click(".composer-saved-draft");
			await browser.until(
				"document.querySelector('.composer-input')?.textContent==='正在写的下一条草稿'",
				"可恢复被保留的草稿",
			);

			await browser.fill(".composer-input", "图片发送失败");
			await browser.evaluate(
				`(()=>{const bytes=Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jHwAAAABJRU5ErkJggg=='),c=>c.charCodeAt(0));const transfer=new DataTransfer();transfer.items.add(new File([bytes],'pixel.png',{type:'image/png'}));const input=document.querySelector('input[type=file]');input.files=transfer.files;input.dispatchEvent(new Event('change',{bubbles:true}));})()`,
			);
			await browser.until(
				"document.querySelectorAll('.draft-image').length===1 && !document.querySelector('[aria-label=发送消息]').disabled",
				"图片存储完成",
			);
			await browser.click('[aria-label="发送消息"]');
			await browser.until("!!document.querySelector('.outgoing-status[data-state=failed]')", "附图发送失败可恢复");
			const imageRequest = writes.at(-1)!;
			assert.equal(imageRequest.input.images.length, 1);
			await browser.evaluate("window.__bubble=document.querySelector('.user-bubble');window.__animationStarts=0");
			await browser.fill(".composer-input", "保留这条新草稿");
			mode = "accept";
			await clickAction("重试");
			await browser.until(
				"!document.querySelector('.outgoing-status') && document.querySelectorAll('.user-bubble').length===1",
				"成功回执承接乐观气泡",
			);
			assert.equal(writes.at(-1)?.input.id, imageRequest.input.id);
			assert.deepEqual(writes.at(-1)?.input.images, imageRequest.input.images);
			assert.equal(
				await browser.evaluate("window.__bubble===document.querySelector('.user-bubble')"),
				true,
				"确认保留同一个气泡节点",
			);
			assert.equal(await browser.evaluate("window.__animationStarts"), 0, "重试与确认不重播发送动效");
			assert.equal(
				await browser.evaluate("document.querySelector('.composer-input').textContent"),
				"保留这条新草稿",
			);

			mode = "unknown";
			await browser.fill(".composer-input", "断线后待核对");
			await browser.click('[aria-label="发送消息"]');
			await browser.until("!!document.querySelector('.outgoing-status[data-state=unknown]')", "连接丢失进入待确认");
			const unknownRequest = writes.at(-1)!;
			await browser.fill(".composer-input", "刷新后继续写");
			const beforeReload = writes.length;
			await browser.navigate(`${origin}/s/s1`);
			await browser.until(
				"!!document.querySelector('.outgoing-status[data-state=unknown]') && document.querySelector('.composer-input')?.textContent==='刷新后继续写'",
				"刷新恢复发送记录和独立草稿",
			);
			assert.equal(writes.length, beforeReload, "恢复不会自动重发");
			assert.equal(
				await browser.evaluate("document.querySelectorAll('.user-line[data-fresh=true]').length"),
				0,
				"刷新不重播气泡动效",
			);
			mode = "accept";
			await clickAction("重试");
			await browser.until(
				"!document.querySelector('.outgoing-status') && document.querySelectorAll('.user-bubble').length===2",
				"手动原样重试成功",
			);
			assert.equal(writes.at(-1)?.input.id, unknownRequest.input.id);

			mode = "lost";
			await browser.fill(".composer-input", "回执丢失但已经接受");
			await browser.click('[aria-label="发送消息"]');
			await browser.until(
				"document.querySelectorAll('.user-bubble').length===3 && !document.querySelector('.outgoing-status')",
				"实时事件先确认，丢失响应不降级",
			);
			assert.equal(sessions.get("s1")?.messages.length, 3);

			await browser.navigate(`${origin}/`);
			await browser.until("!!document.querySelector('.composer-input')", "回到首页");
			holdCreation = true;
			mode = "unknown";
			await browser.fill(".composer-input", "首页首条消息");
			await browser.click('[aria-label="发送消息"]');
			await browser.until(
				"!!document.querySelector('.home-outgoing .user-bubble') && document.querySelector('.composer-input')?.textContent===''",
				"首页创建期间乐观展示",
			);
			holdCreation = false;
			releaseCreation?.();
			await browser.until(
				"location.pathname==='/s/s2' && !!document.querySelector('.outgoing-status[data-state=unknown]')",
				"创建成功后接管工作台，发送失败仍保留",
			);
			mode = "accept";
			await clickAction("重试");
			await browser.until(
				"!document.querySelector('.outgoing-status') && document.querySelectorAll('.user-bubble').length===1",
				"首条消息复用原会话重试",
			);
			assert.equal(creationWrites, 1);
			assert.equal(sessions.size, 2);

			mode = "reject";
			await browser.fill(".composer-input", "检查失败状态的浅深主题与窄屏布局：长错误和操作按钮应当完整显示。");
			await browser.click('[aria-label="发送消息"]');
			await browser.until("!!document.querySelector('.outgoing-status[data-state=failed]')", "失败状态截图就绪");
			for (const theme of ["light", "dark"])
				for (const width of [1440, 390]) {
					await browser.call("Emulation.setDeviceMetricsOverride", {
						width,
						height: 900,
						deviceScaleFactor: 1,
						mobile: false,
					});
					await browser.evaluate(
						`document.documentElement.dataset.theme=${JSON.stringify(theme)}; document.querySelector(${JSON.stringify(width === 390 ? ".sidebar-backdrop" : ".sidebar-restore")})?.click()`,
					);
					await browser.evaluate(
						"Promise.all(document.getAnimations().filter(a=>a.effect?.getTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{})))",
					);
					await browser.screenshot(join(root, `send-failed-${theme}-${width}.png`));
					assert.equal(
						await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"),
						true,
						"操作栏不撑开页面",
					);
				}
			await browser.call("Emulation.setEmulatedMedia", {
				features: [{ name: "prefers-reduced-motion", value: "reduce" }],
			});
			await browser.evaluate("document.querySelector('.sidebar-backdrop')?.click()");
			await browser.fill(".composer-input", "减少动态时发送");
			await browser.click('[aria-label="发送消息"]');
			await browser.until("document.querySelectorAll('.user-bubble').length===3", "减少动态仍立即显示消息");
			assert.equal(
				await browser.evaluate(
					"[...document.querySelectorAll('.user-bubble')].every(e=>e.getAnimations().length===0)",
				),
				true,
			);
			const beforeRecovery = writes.length;
			const recoveredSession = sessions.get("s2");
			assert.ok(recoveredSession);
			recoveredSession.receipts.push({
				id: "recovered",
				text: "宿主中断前的消息",
				status: "delivering",
				delivery: "direct",
				sequence: 99,
				images: [],
				uncertain: true,
			});
			await browser.navigate(`${origin}/s/s2`);
			await browser.until(
				"document.querySelector('.queue-panel')?.textContent.includes('上次投递结果待核对')",
				"宿主中断的投递结果可见且不误报失败",
			);
			assert.equal(
				await browser.evaluate(
					"[...document.querySelectorAll('.queue-item')].find(e=>e.textContent.includes('宿主中断前的消息')).querySelectorAll('button').length",
				),
				0,
				"不为未知投递提供直接重发入口",
			);
			assert.equal(writes.length, beforeRecovery);
		} finally {
			release?.();
			releaseCreation?.();
			await browser?.close();
			for (const set of clients.values()) for (const response of set) response.end();
			server.closeAllConnections();
			await new Promise<void>((resolve) => server.close(() => resolve()));
		}
	},
);
