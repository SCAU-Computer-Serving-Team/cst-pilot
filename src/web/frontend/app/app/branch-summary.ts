/**
 * 分支总结的跨页约定：树页把目标条目与模式写进地址参数，聊天页接着完成。
 * 参数留在地址里，刷新后待办仍在；聊天页开始请求或取消时清掉。
 */
export type BranchSummaryChoice = { entryId: string; mode: "summarize" | "custom" };

/** 输入框里标记块的文案，树页与聊天页共用。 */
export const summaryTagText = "自定义总结提示词";

export function branchSummaryChoice(params: URLSearchParams): BranchSummaryChoice | undefined {
	const entryId = params.get("branch");
	const mode = params.get("mode");
	if (!entryId || (mode !== "summarize" && mode !== "custom")) return undefined;
	return { entryId, mode };
}

export function branchSummaryHref(sessionId: string, entryId: string, mode: BranchSummaryChoice["mode"]): string {
	return `/s/${encodeURIComponent(sessionId)}?branch=${encodeURIComponent(entryId)}&mode=${mode}`;
}

export type NavigateResult = { cancelled: boolean; editorText?: string; summaryEntryId?: string };

/**
 * 导航与总结必须一次调用完成：pi 的总结取自「当前叶指针到目标条目」那段分支，
 * 叶指针一移动，那段分支就找不回来了。自定义提示词因此要在输入框里收好再发这一次请求。
 */
export function summaryBody(
	entryId: string,
	customInstructions?: string,
): { entryId: string; summarize: true; customInstructions?: string } {
	return customInstructions ? { entryId, summarize: true, customInstructions } : { entryId, summarize: true };
}
