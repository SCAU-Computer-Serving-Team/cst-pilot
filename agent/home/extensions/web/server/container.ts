import type { Server } from "node:http";

const key = Symbol.for("cst-pilot.web.container");

export interface WebContainer {
	server?: Server;
	starting?: Promise<void>;
	/** Reserved for the per-session loader window in checkpoint 2. */
	loadingWebSession: boolean;
}

export function getWebContainer(): WebContainer {
	const globals = globalThis as typeof globalThis & { [key]?: WebContainer };
	if (!globals[key]) globals[key] = { loadingWebSession: false };
	return globals[key];
}
