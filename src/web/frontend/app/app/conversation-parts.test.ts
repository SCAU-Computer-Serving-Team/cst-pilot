import assert from "node:assert/strict";
import { test } from "node:test";
import type { ContentPart } from "./web-state.ts";
import { orderedParts, visibleQueueItems } from "./conversation-parts.ts";

const call = (id: string, name = "disk"): ContentPart => ({ type: "toolCall", id, name, arguments: {} });
test("assistant prose stays before and after the corresponding tool calls", () => {
  const parts = orderedParts([
    { type: "thinking", thinking: "why" },
    { type: "text", text: "下面联网搜索这两款的详细资料：" },
    call("a", "web_search"),
    { type: "text", text: "查询后的结论" },
    call("b"), call("c"),
  ]);
  assert.deepEqual(parts.map((part) => part.kind), ["thinking", "text", "tools", "text", "tools"]);
  assert.equal(parts[1].kind === "text" && parts[1].text, "下面联网搜索这两款的详细资料：");
  assert.equal(parts[4].kind === "tools" && parts[4].calls.length, 2);
});

test("a direct submission is never rendered as a queued follow-up", () => {
  const make = (status: "pending" | "failed", id: string, delivery: "direct" | "queue") => ({ id, status, delivery, text: id, sequence: 1, images: [] });
  assert.deepEqual(visibleQueueItems([make("pending", "first", "direct")]), []);
  assert.deepEqual(visibleQueueItems([make("pending", "later", "queue")]).map((item) => item.id), ["later"]);
  assert.deepEqual(visibleQueueItems([make("pending", "first", "direct"), make("pending", "later", "queue")]).map((item) => item.id), ["later"]);
  assert.deepEqual(visibleQueueItems([make("failed", "no-message", "direct")]).map((item) => item.id), ["no-message"]);
});
