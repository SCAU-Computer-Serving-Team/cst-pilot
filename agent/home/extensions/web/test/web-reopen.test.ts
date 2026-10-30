import assert from "node:assert/strict";
import childProcess from "node:child_process";
import { EventEmitter } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { syncBuiltinESMExports } from "node:module";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import {
	type CustomEditor,
	DefaultResourceLoader,
	type ExtensionContext,
	type ExtensionUIContext,
} from "@earendil-works/pi-coding-agent";
import type { TUI } from "@earendil-works/pi-tui";
import { KeybindingsManager } from "../../../../node_modules/@earendil-works/pi-coding-agent/dist/core/keybindings.js";
import { getWebContainer } from "../server/container.ts";
import { testRoot } from "./support.ts";

test("Web 待机终端可重复输入 /web 打开浏览器，保持原服务且不分发其他命令", async (t) => {
	const home = await mkdtemp(join(await testRoot(), "web-reopen-"));
	const loader = new DefaultResourceLoader({
		cwd: home,
		agentDir: home,
		noContextFiles: true,
		noExtensions: true,
		noSkills: true,
		noThemes: true,
		noPromptTemplates: true,
		additionalExtensionPaths: [fileURLToPath(new URL("../index.ts", import.meta.url))],
	});
	await loader.reload();
	assert.deepEqual(loader.getExtensions().errors, []);
	const extension = loader.getExtensions().extensions[0];
	const container = getWebContainer();
	const server = createServer();
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	container.server = server;
	container.parked = true;
	const previous = process.env.CST_WEB_NO_OPEN;
	delete process.env.CST_WEB_NO_OPEN;
	const launch = t.mock.method(childProcess, "spawn", () => Object.assign(new EventEmitter(), { unref() {} }));
	syncBuiltinESMExports();
	t.after(async () => {
		container.releaseTerminalInput?.();
		container.releaseTerminalInput = undefined;
		container.server = undefined;
		container.parked = false;
		launch.mock.restore();
		syncBuiltinESMExports();
		if (previous === undefined) delete process.env.CST_WEB_NO_OPEN;
		else process.env.CST_WEB_NO_OPEN = previous;
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	});
	const start = extension.handlers.get("session_start")?.[0];
	const listeners: Parameters<ExtensionUIContext["onTerminalInput"]>[0][] = [];
	let editor: CustomEditor | undefined;
	let dispatched = 0;
	const notifications: string[] = [];
	const keys = new KeybindingsManager();
	const tui = { requestRender() {}, terminal: { columns: 100, rows: 30 } } as unknown as TUI;
	const ui = {
		onTerminalInput(handler) {
			listeners.push(handler);
			return () => listeners.splice(listeners.indexOf(handler), 1);
		},
		setEditorComponent(factory) {
			if (!factory) return;
			editor = factory(
				tui,
				{
					borderColor: (text) => text,
					selectList: {
						selectedPrefix: (text) => text,
						selectedText: (text) => text,
						description: (text) => text,
						scrollInfo: (text) => text,
						noMatch: (text) => text,
					},
				},
				keys,
			) as CustomEditor;
			// Pi 在安装自定义编辑器后会复制这些原生回调。
			editor.onSubmit = () => dispatched++;
			editor.onCtrlD = () => dispatched++;
			editor.onEscape = () => dispatched++;
			editor.onAction("app.model.select", () => dispatched++);
		},
		getEditorText: () => editor?.getText() ?? "",
		setEditorText: (text) => editor?.setText(text),
		setWidget() {},
		notify: (message) => notifications.push(message),
	} satisfies Partial<ExtensionUIContext>;
	assert.ok(start);
	await start({ type: "session_start" }, { mode: "tui", ui } as unknown as ExtensionContext);
	assert.ok(editor);
	const input = (data: string) => {
		for (const listener of listeners) {
			if (listener(data)?.consume) return;
		}
		editor?.handleInput(data);
	};
	for (let attempt = 0; attempt < 2; attempt++) {
		for (const char of "/web") input(char);
		assert.equal(editor.getText(), "/web", "待机输入应显示 /web");
		input("\r");
		assert.equal(launch.mock.callCount(), attempt + 1, "提交 /web 应重新打开浏览器");
		assert.equal(editor.getText(), "");
		assert.equal(container.server, server);
		assert.equal(server.listening, true);
		assert.equal(container.parked, true);
	}
	for (const command of ["/new", "/model", "/quit", "普通消息"]) {
		input(command);
		input("\r");
	}
	for (const key of ["\x03", "\x04", "\x1b", "\x0c"]) input(key);
	assert.equal(dispatched, 0, "不能调用 Pi 原生提交、退出或模型菜单");
	assert.equal(launch.mock.callCount(), 2);
	assert.ok(notifications.length > 0, "其他输入应提示仅支持 /web");

	// 首次 session_start 和移交完成后都会安装待机界面，旧监听必须被替换。
	await start({ type: "session_start" }, { mode: "tui", ui } as unknown as ExtensionContext);
	assert.equal(listeners.length, 1);
	input("\x1b[200~/wex\x1b[201~");
	input("\x7f");
	input("b");
	input("\r");
	assert.equal(launch.mock.callCount(), 3, "重新安装后仍支持粘贴、退格和提交 /web");
	assert.equal(dispatched, 0);
	assert.equal(container.server, server);
});
