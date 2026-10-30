import assert from "node:assert/strict";
import { test } from "node:test";
import { deepseekFlash, supplementDeepSeekFlash } from "../model-catalog.mjs";
import { RELEASE_SETTINGS, RELEASE_WEB_SEARCH } from "../release-settings.mjs";

test("portable startup selects the official DeepSeek Flash default and free Exa search", () => {
  assert.equal(RELEASE_SETTINGS.theme, "light/dark");
  assert.equal(RELEASE_SETTINGS.defaultProvider, "deepseek");
  assert.equal(RELEASE_SETTINGS.defaultModel, "deepseek-flash");
  assert.equal(RELEASE_SETTINGS.defaultModel, deepseekFlash.id);
  assert.equal(RELEASE_WEB_SEARCH.searchProvider, "exa");
  assert.deepEqual(RELEASE_SETTINGS.packages, ["./packages/pi-fff", "./packages/pi-open-tui", "./packages/pi-web-access"]);
});

test("offline catalog supplies the official default without touching other providers", () => {
  const existing = { deepseek: { models: [{ id: "older" }], checkedAt: 1 }, other: { models: [] } };
  const updated = supplementDeepSeekFlash(existing, 2);
  assert.deepEqual(updated.deepseek.models.map((model) => model.id), ["older", "deepseek-flash"]);
  assert.equal(updated.deepseek.models[1].baseUrl, "https://api.deepseek.com");
  assert.equal(updated.deepseek.models[1].provider, "deepseek");
  assert.equal(updated.other, existing.other);
  assert.equal(updated.deepseek.lastModified, 2);
  assert.equal(supplementDeepSeekFlash(updated, 2), updated);
  assert.equal(deepseekFlash.name, "DeepSeek V4.1 Flash");
  assert.deepEqual(deepseekFlash.cost, { input: 0.3, output: 1.2, cacheRead: 0.006, cacheWrite: 0 });
  assert.equal(deepseekFlash.compat.supportsStrictMode, true);
  assert.equal(deepseekFlash.inputLimits.images.resize.maxBytes, 4718592);
});

test("an upstream default entry keeps its metadata and freshness timestamp", () => {
  const upstream = { ...deepseekFlash, name: "Upstream Flash", cost: { ...deepseekFlash.cost, input: 0.1 } };
  const catalog = { deepseek: { models: [upstream], lastModified: 2, checkedAt: 3, etag: "upstream" } };
  assert.equal(supplementDeepSeekFlash(catalog, 4), catalog);
  assert.equal(catalog.deepseek.models[0], upstream);
  assert.equal(catalog.deepseek.lastModified, 2);
});
