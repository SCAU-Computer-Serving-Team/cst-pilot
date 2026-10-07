import { readFileSync } from "node:fs";
import { resolve } from "node:path";

/** 发行页眉与包内版本一致；开发仓库未生成VERSION时明确显示dev。 */
export function readKitVersion(file?: URL): string {
	const agentDir = process.env.PI_CODING_AGENT_DIR;
	const target = file ?? (agentDir ? resolve(agentDir, "../../VERSION") : undefined);
	if (!target) return "dev";
	try {
		return /^cst-pilot ([a-zA-Z0-9][a-zA-Z0-9.-]{0,63})$/m.exec(readFileSync(target, "utf8"))?.[1] ?? "dev";
	} catch {
		return "dev";
	}
}
