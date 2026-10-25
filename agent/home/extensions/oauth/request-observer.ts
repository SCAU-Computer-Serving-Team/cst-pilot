import { oaHost } from "./oa.ts";

type Finish = (requestId?: string) => void;
type Start = () => Finish;
const KEY = Symbol.for("cst-pilot/cstoa-requests");

function observers(): Map<string, Start> {
	const global = globalThis as Record<symbol, unknown>;
	global[KEY] ??= new Map<string, Start>();
	return global[KEY] as Map<string, Start>;
}

/** 仅传输请求元数据。没有订阅者时直接使用原 fetch，不采集。 */
export function observeRequests(sessionId: string, start: Start): () => void {
	observers().set(sessionId, start);
	return () => {
		if (observers().get(sessionId) === start) observers().delete(sessionId);
	};
}

export function requestFetch(sessionId: string | undefined, fetcher: typeof fetch = fetch): typeof fetch {
	const start = sessionId ? observers().get(sessionId) : undefined;
	if (!start) return fetcher;
	return async (input, init) => {
		const url = new URL(input instanceof Request ? input.url : String(input));
		if (url.origin !== new URL(oaHost()).origin || !url.pathname.endsWith("/chat/completions"))
			return fetcher(input, init);
		let finish: Finish | undefined;
		try {
			finish = start();
		} catch {
			/* 采集故障不影响推理。 */
		}
		try {
			const response = await fetcher(input, init);
			const id = response.headers.get("x-oneapi-request-id") ?? undefined;
			try {
				finish?.(id && /^[A-Za-z0-9_-]{1,128}$/.test(id) ? id : undefined);
			} catch {
				/* 同上。 */
			}
			return response;
		} catch (error) {
			try {
				finish?.();
			} catch {
				/* 同上。 */
			}
			throw error;
		}
	};
}
