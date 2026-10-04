/** 输入草稿的 sessionStorage 命名空间：按会话隔离，新会话固定 new；消息 id 与图片挂同键的 :id / :images 后缀。 */
export const draftKey = (sessionId?: string) => `cst-draft:${sessionId ?? "new"}`;
