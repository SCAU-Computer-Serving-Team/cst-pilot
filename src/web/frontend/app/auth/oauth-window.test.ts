import assert from "node:assert/strict";
import { test } from "node:test";
import { finishAuthorization, hasLocalCallback, openAuthorization } from "./oauth-window.ts";

test("授权窗先断开 opener，再跳转；成功关闭并返回原页面", () => {
	const actions: string[] = [];
	const popup = {
		opener: "old",
		closed: false,
		location: {
			replace: (url: string) => {
				assert.equal(popup.opener, null);
				actions.push(url);
			},
		},
		close: () => actions.push("close"),
	} as unknown as Window;
	const browser = {
		open: (url?: string | URL) => {
			actions.push(String(url));
			return popup;
		},
		focus: () => actions.push("focus"),
	};
	assert.equal(openAuthorization("https://example.test/login", browser), popup);
	finishAuthorization(popup, browser);
	assert.deepEqual(actions, ["about:blank", "https://example.test/login", "close", "focus"]);
});

test("弹窗拦截与跨源隔离不影响原页成功处理", () => {
	assert.equal(openAuthorization("https://example.test", { open: () => null }), null);
	assert.equal(
		openAuthorization("javascript:alert(1)", {
			open: () => {
				throw new Error("不得打开");
			},
		}),
		null,
	);
	let focused = false;
	finishAuthorization(
		{
			get closed(): boolean {
				throw new Error("隔离");
			},
		} as Window,
		{
			focus: () => {
				focused = true;
			},
		},
	);
	assert.equal(focused, true);
});

test("只将回环回调识别为自动回调，不改写供应商 redirect_uri", () => {
	for (const address of ["http://localhost:53692/callback", "http://127.0.0.1:1234/oauth/callback/a"]) {
		assert.equal(hasLocalCallback(`https://example.test/auth?redirect_uri=${encodeURIComponent(address)}`), true);
	}
	assert.equal(hasLocalCallback("https://example.test/auth?callback_url=http%3A%2F%2Flocalhost%3A1%2Fcallback"), true);
	assert.equal(hasLocalCallback("https://example.test/auth?redirect_uri=https%3A%2F%2Felsewhere.test"), false);
	assert.equal(hasLocalCallback("broken"), false);
});
