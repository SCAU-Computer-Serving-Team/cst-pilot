import { randomUUID } from "node:crypto";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { ConfigurationUnavailable } from "../../../runtime/configuration.ts";
import { replaceFile } from "./replace-file.ts";

export type WebTheme = "light" | "dark" | "system";
/** Web主题与空模型选择独立保存；Pi 的空 enabledModels 表示默认范围。 */
export class WebSettings {
	private readonly file: string;
	private tail: Promise<void> = Promise.resolve();
	constructor(agentDir: string) {
		this.file = join(agentDir, "web-settings.json");
	}
	async read(replacementTheme?: WebTheme): Promise<{ theme: WebTheme; emptyModelScope: boolean }> {
		try {
			const value: unknown = JSON.parse(await readFile(this.file, "utf8"));
			if (!value || typeof value !== "object" || Array.isArray(value))
				throw new ConfigurationUnavailable("Web外观配置无法读取，请检查 web-settings.json 后重试。");
			const theme = "theme" in value ? value.theme : undefined;
			const validTheme = typeof theme === "string" && ["light", "dark", "system"].includes(theme);
			if (!validTheme && replacementTheme === undefined)
				throw new ConfigurationUnavailable("Web外观配置无法读取，请检查 web-settings.json 后重试。");
			return {
				theme: validTheme ? (theme as WebTheme) : replacementTheme!,
				emptyModelScope: "emptyModelScope" in value && value.emptyModelScope === true,
			};
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") return { theme: "light", emptyModelScope: false };
			if (error instanceof SyntaxError)
				throw new ConfigurationUnavailable("Web外观配置无法读取，请检查 web-settings.json 后重试。");
			throw error;
		}
	}
	private async update(change: Partial<{ theme: WebTheme; emptyModelScope: boolean }>) {
		const task = this.tail.then(async () => {
			const next = { ...(await this.read(change.theme)), ...change };
			await mkdir(join(this.file, ".."), { recursive: true });
			const temp = `${this.file}.${randomUUID()}.tmp`;
			try {
				await writeFile(temp, JSON.stringify(next), { flag: "wx" });
				await replaceFile(temp, this.file);
			} finally {
				await rm(temp, { force: true });
			}
			return next;
		});
		this.tail = task.then(
			() => undefined,
			() => undefined,
		);
		return task;
	}
	async save(theme: WebTheme) {
		return this.update({ theme });
	}
	async saveEmptyModelScope(emptyModelScope: boolean) {
		return this.update({ emptyModelScope });
	}
}
