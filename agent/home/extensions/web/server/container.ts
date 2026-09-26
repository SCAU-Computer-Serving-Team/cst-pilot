import type { Server } from "node:http";
import type { WebSessionPool } from "./sessions.ts";

const key = Symbol.for("cst-pilot.web.container");

export interface WebContainer {
	server?: Server;
	starting?: Promise<void>;
	loadingWebSession: number;
	parked: boolean;
	pool?: WebSessionPool;
	closeApi?: () => Promise<void>;
}

export function getWebContainer(): WebContainer {
	const globals = globalThis as typeof globalThis & { [key]?: WebContainer };
	if (!globals[key]) globals[key] = { loadingWebSession: 0, parked: false };
	return globals[key];
}
