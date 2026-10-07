import { randomUUID } from "node:crypto";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { ConfigurationUnavailable } from "../../../runtime/configuration.ts";
import { replaceFile } from "./replace-file.ts";

export type WebTheme = "light" | "dark" | "system";
/** Web外观独立于Pi settings.json，缺省浅色。每次读取文件，写入串行且原子替换。 */
export class WebSettings {
	private readonly file: string;
	private tail: Promise<void> = Promise.resolve();
	constructor(agentDir: string) {
		this.file = join(agentDir, "web-settings.json");
	}
	async read(): Promise<{ theme: WebTheme }> {
		try {
			const value: unknown = JSON.parse(await readFile(this.file, "utf8"));
			if (
				!value ||
				typeof value !== "object" ||
				!("theme" in value) ||
				!["light", "dark", "system"].includes(String(value.theme))
			)
				throw new ConfigurationUnavailable("Web外观配置无法读取，请检查 web-settings.json 后重试。");
			return { theme: value.theme as WebTheme };
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") return { theme: "light" };
			if (error instanceof SyntaxError)
				throw new ConfigurationUnavailable("Web外观配置无法读取，请检查 web-settings.json 后重试。");
			throw error;
		}
	}
	async save(theme: WebTheme): Promise<{ theme: WebTheme }> {
		const task = this.tail.then(async () => {
			await mkdir(join(this.file, ".."), { recursive: true });
			const temp = `${this.file}.${randomUUID()}.tmp`;
			try {
				await writeFile(temp, JSON.stringify({ theme }), { flag: "wx" });
				await replaceFile(temp, this.file);
			} finally {
				await rm(temp, { force: true });
			}
			return { theme };
		});
		this.tail = task.then(
			() => undefined,
			() => undefined,
		);
		return task;
	}
}
