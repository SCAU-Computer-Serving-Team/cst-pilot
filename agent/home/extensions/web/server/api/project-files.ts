import { type Dirent, readdirSync } from "node:fs";
import { join } from "node:path";

const CACHE_MS = 30_000;
const LIMIT = 4_000;
let cache: { at: number; root: string; files: string[] } | null = null;

/** Skip dependencies and build outputs; limit traversal to six levels and 4000 files. */
export function listProjectFiles(root: string): string[] {
	if (cache && cache.root === root && Date.now() - cache.at < CACHE_MS) return cache.files;
	const files: string[] = [];
	const skip = new Set(["node_modules", ".git", "dist", "build", "out", "coverage", "vendor", "tmp"]);
	const walk = (dir: string, prefix: string, depth: number): void => {
		if (files.length >= LIMIT || depth > 6) return;
		let entries: Dirent[];
		try {
			entries = readdirSync(dir, { withFileTypes: true });
		} catch {
			return;
		}
		for (const entry of entries) {
			if (files.length >= LIMIT) return;
			if (entry.name.startsWith(".")) continue;
			if (entry.isDirectory()) {
				if (!skip.has(entry.name)) walk(join(dir, entry.name), `${prefix}${entry.name}/`, depth + 1);
				continue;
			}
			if (entry.isFile()) files.push(`${prefix}${entry.name}`);
		}
	};
	walk(root, "", 0);
	files.sort();
	cache = { at: Date.now(), root, files };
	return files;
}
