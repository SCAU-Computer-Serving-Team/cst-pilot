import { rename } from "node:fs/promises";

/** 保持原子替换。Windows读文件/扫描期间的短占用最多重试375ms，不删除原文件。 */
export async function replaceFile(source: string, target: string, move: typeof rename = rename): Promise<void> {
	for (let attempt = 0; ; attempt++) {
		try {
			await move(source, target);
			return;
		} catch (error) {
			const code = (error as NodeJS.ErrnoException).code;
			if (attempt >= 4 || !["EPERM", "EACCES", "EBUSY"].includes(code ?? "")) throw error;
			await new Promise((resolve) => setTimeout(resolve, 25 * 2 ** attempt));
		}
	}
}
