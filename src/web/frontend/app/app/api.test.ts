import assert from "node:assert/strict";
import { test } from "node:test";
import { apiJson } from "./api.ts";

test("API writes carry the local request header and a deduplication key", async () => {
  const original = globalThis.fetch;
  let request: Request | undefined;
  globalThis.fetch = async (input, init) => {
    request = new Request(new URL(String(input), "http://127.0.0.1:52831"), init);
    return Response.json({ providerId: "probe", type: "api_key" });
  };
  try {
    const result = await apiJson<{ providerId: string }>("/api/auth/probe/api-key", {
      method: "PUT",
      body: { key: "test-secret" },
    });
    assert.equal(result.providerId, "probe");
    assert.equal(request?.method, "PUT");
    assert.equal(request?.headers.get("X-CST-Web-Request"), "1");
    assert.match(request?.headers.get("Idempotency-Key") ?? "", /^[0-9a-f-]{36}$/);
    assert.equal(request?.headers.get("Content-Type"), "application/json");
    assert.deepEqual(await request?.json(), { key: "test-secret" });
  } finally {
    globalThis.fetch = original;
  }
});

test("API errors use the server message without exposing submitted credentials", async () => {
  const original = globalThis.fetch;
  globalThis.fetch = async () => Response.json({ error: { code: "invalid_request", message: "请求的密钥格式无效" } }, { status: 400 });
  try {
    await assert.rejects(apiJson("/api/auth/probe/api-key", { method: "PUT", body: { key: "test-secret" } }),
      (error: Error) => error.message === "请求的密钥格式无效" && !error.message.includes("test-secret"));
  } finally {
    globalThis.fetch = original;
  }
});

test("network errors give an actionable local-service message", async () => {
  const original = globalThis.fetch;
  globalThis.fetch = async () => { throw new TypeError("Failed to fetch"); };
  try {
    await assert.rejects(apiJson("/api/auth"), /请确认 pi 仍在运行/);
  } finally {
    globalThis.fetch = original;
  }
});
