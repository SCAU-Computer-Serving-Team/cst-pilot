import { apiJson, sessionPath } from "../data/api.ts";

export type DeletionTarget = { id: string; title: string };
export type DeletionResult = {
	deleted: DeletionTarget[];
	failed: (DeletionTarget & { error: string })[];
};

async function deleteSavedSession(id: string): Promise<void> {
	const result = await apiJson<{ deleted: boolean }>(sessionPath(id), { method: "DELETE" });
	if (result?.deleted !== true) throw new Error("删除结果未确认");
}

/** 删除共用会话索引，逐项提交；一项失败不阻止后续项，也不自动重试。 */
export async function deleteSessions(
	targets: readonly DeletionTarget[],
	remove: (id: string) => Promise<void> = deleteSavedSession,
): Promise<DeletionResult> {
	const result: DeletionResult = { deleted: [], failed: [] };
	const seen = new Set<string>();
	for (const target of targets) {
		if (seen.has(target.id)) continue;
		seen.add(target.id);
		try {
			await remove(target.id);
			result.deleted.push(target);
		} catch (cause) {
			result.failed.push({
				...target,
				error: cause instanceof Error ? cause.message : "删除失败",
			});
		}
	}
	return result;
}

export function deletionTitle(result: DeletionResult): string {
	if (!result.deleted.length) return "删除失败";
	if (!result.failed.length) return `已删除 ${result.deleted.length} 个会话`;
	return `已删除 ${result.deleted.length} 个，${result.failed.length} 个失败`;
}

export function deletionReason(error: string): string {
	return error === "会话正在执行" ? "正在运行" : error;
}
