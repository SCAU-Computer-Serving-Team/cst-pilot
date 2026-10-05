/** 授权窗不保留 opener，供应商页面无法改写原页面。 */
export function openAuthorization(url: string, browser: Pick<Window, "open"> = window): Window | null {
	const parsed = new URL(url);
	if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return null;
	const popup = browser.open("about:blank", "_blank", "popup,width=720,height=820");
	if (!popup) return null;
	try {
		popup.opener = null;
		popup.location.replace(parsed.href);
		return popup;
	} catch {
		popup.close();
		return null;
	}
}

/** 登录结果以服务器为准，回调页本身不代表令牌已保存。 */
export function finishAuthorization(popup: Window | null, browser: Pick<Window, "focus"> = window) {
	try {
		if (popup && !popup.closed) popup.close();
	} catch {
		/* 供应商的跨源隔离可能切断窗口引用。 */
	}
	browser.focus();
}

export function hasLocalCallback(authUrl: string): boolean {
	try {
		const url = new URL(authUrl);
		const callback = url.searchParams.get("redirect_uri") ?? url.searchParams.get("callback_url");
		if (!callback) return false;
		const target = new URL(callback);
		return target.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(target.hostname);
	} catch {
		return false;
	}
}
