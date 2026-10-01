import type { ContentPart, Message, QueueItem } from "./web-state";

type Thinking = Extract<ContentPart, { type: "thinking" }>;
type Call = Extract<ContentPart, { type: "toolCall" }>;
export type ConversationPart = { kind: "thinking"; part: Thinking } | { kind: "text"; text: string } | { kind: "tools"; calls: Call[] };

/** The model can put prose before and after a tool call in the same assistant message. */
export function orderedParts(content: ContentPart[]): ConversationPart[] {
  const parts: ConversationPart[] = [];
  for (const part of content) {
    const last = parts.at(-1);
    if (part.type === "thinking") parts.push({ kind: "thinking", part });
    else if (part.type === "toolCall") {
      if (last?.kind === "tools") last.calls.push(part);
      else parts.push({ kind: "tools", calls: [part] });
    } else if (part.type === "text") {
      if (last?.kind === "text") last.text += `\n${part.text}`;
      else parts.push({ kind: "text", text: part.text });
    }
  }
  return parts;
}

/** The server classifies each accepted input; never infer queue membership from timing. */
export function visibleQueueItems(items: QueueItem[]): QueueItem[] {
  return items.filter((item) => item.status === "failed" || item.status === "pending" && item.delivery !== "direct");
}

export type TurnEntry = { id?: string; message: Message };
/** 一轮回答：一条用户消息加上其后全部助手消息（agent 循环里一轮可含多条助手消息）。分支总结自成一块。 */
export type Turn = { user?: TurnEntry; assistants: TurnEntry[]; summary?: TurnEntry };

/** 按用户消息切分轮次；开头的孤立助手消息（例如被截断的历史）自成一轮，分支总结独占一块。 */
export function groupTurns(entries: TurnEntry[]): Turn[] {
  const turns: Turn[] = [];
  for (const entry of entries) {
    if (entry.message.role === "user") turns.push({ user: entry, assistants: [] });
    else if (entry.message.role === "branchSummary") turns.push({ summary: entry, assistants: [] });
    else {
      const last = turns.at(-1);
      if (last && !last.summary) last.assistants.push(entry);
      else turns.push({ assistants: [entry] });
    }
  }
  return turns;
}

const settled = (turn: Turn | undefined): boolean => {
  const stop = turn?.assistants.at(-1)?.message.stopReason;
  return !!stop && stop !== "toolUse";
};

/** 运行中正在推进的一轮：最后一轮已有助手内容就是最后一轮；用户刚发的空轮次在上一轮已结束时亮出等待态，否则仍是上一轮（插话排队）。 */
export function activeTurnIndex(turns: Turn[], running: boolean): number {
  if (!running || turns.length === 0) return -1;
  let last = turns.length - 1;
  while (last >= 0 && turns[last]!.summary) last -= 1; // 分支总结块不参与活动轮次判定
  if (last < 0) return -1;
  if (turns[last]!.assistants.length > 0) return last;
  let previous = last - 1;
  while (previous >= 0 && turns[previous]!.summary) previous -= 1;
  if (previous < 0) return last;
  return settled(turns[previous]) ? last : previous;
}

export type TurnPart = { entry: TurnEntry; final: boolean; part: ConversationPart };

/** 一轮里除最后的正文以外都是过程内容；最后的正文 = 最后一条助手消息的末尾非空文本段。 */
export function splitTurn(assistants: TurnEntry[]): { process: TurnPart[]; finalText: string } {
  const process: TurnPart[] = [];
  let finalText = "";
  assistants.forEach((entry, index) => {
    const final = index === assistants.length - 1;
    const content: ContentPart[] = typeof entry.message.content === "string" ? [{ type: "text", text: entry.message.content }] : entry.message.content;
    const parts = orderedParts(content);
    parts.forEach((part, partIndex) => {
      if (final && partIndex === parts.length - 1 && part.kind === "text" && part.text.trim()) {
        finalText = part.text;
        return;
      }
      if (part.kind === "text" && !part.text.trim()) return;
      process.push({ entry, final, part });
    });
  });
  return { process, finalText };
}

/** 轮次耗时：不足 1 秒写「不足 1 秒」，超过 1 分钟写「N 分 M 秒」。 */
export function formatDuration(ms: number): string {
  const seconds = Math.round(Math.max(0, ms) / 1000);
  if (seconds < 1) return "不足 1 秒";
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return rest ? `${minutes} 分 ${rest} 秒` : `${minutes} 分钟`;
}
