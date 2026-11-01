/** 输入草稿的 sessionStorage 命名空间：按会话隔离，新会话固定 new；消息 id 与图片挂同键的 :id / :images 后缀。 */
export const draftKey = (sessionId?: string) => `cst-draft:${sessionId ?? "new"}`;

export type SavedDraft = { text: string; images: string[] };
export function readSavedDrafts(storage: Pick<Storage, "getItem">, key: string): SavedDraft[] {
	const value: unknown = JSON.parse(storage.getItem(`${key}:saved`) ?? "[]");
	if (
		!Array.isArray(value) ||
		!value.every(
			(item: unknown) =>
				item &&
				typeof item === "object" &&
				"text" in item &&
				typeof item.text === "string" &&
				"images" in item &&
				Array.isArray(item.images) &&
				item.images.every((hash: unknown) => typeof hash === "string"),
		)
	)
		throw new Error("保留的草稿记录损坏");
	return value;
}
