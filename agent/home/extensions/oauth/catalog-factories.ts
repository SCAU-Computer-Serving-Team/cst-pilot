import { resolve } from "node:path";
import type { Provider } from "@earendil-works/pi-ai";

const key = Symbol.for("cst-pilot/catalog-provider-factories");
const holder = globalThis as typeof globalThis & { [key]?: Map<string, Map<string, () => Provider>> };
const factories = holder[key] ?? new Map<string, Map<string, () => Provider>>();
holder[key] = factories;

/** Provider 工厂登记不依赖 runtime 扩展，OAuth 扩展可以独立加载。 */
export function catalogProviderFactories(home: string): Map<string, () => Provider> {
	const path = resolve(home);
	const id = process.platform === "win32" ? path.toLowerCase() : path;
	let entries = factories.get(id);
	if (!entries) {
		entries = new Map();
		factories.set(id, entries);
	}
	return entries;
}
