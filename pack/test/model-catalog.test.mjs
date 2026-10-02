import assert from "node:assert/strict";
import { test } from "node:test";
import { goFlash, supplementGoFlash } from "../model-catalog.mjs";
import { RELEASE_SETTINGS } from "../release-settings.mjs";

test("portable startup selects a readable dark theme and the Go Flash default", () => {
  assert.equal(RELEASE_SETTINGS.theme, "dark");
  assert.equal(RELEASE_SETTINGS.defaultProvider, "opencode-go");
  assert.equal(RELEASE_SETTINGS.defaultModel, goFlash.id);
  assert.deepEqual(RELEASE_SETTINGS.packages, ["./packages/pi-fff", "./packages/pi-open-tui", "./packages/pi-web-access"]);
});

test("offline catalog supplies the OpenCode Go default without touching other models", () => {
  const existing = { "opencode-go": { models: [{ id: "older" }], checkedAt: 1 }, deepseek: { models: [] } };
  const updated = supplementGoFlash(existing, 2);
  assert.deepEqual(updated["opencode-go"].models.map((model) => model.id), ["older", "deepseek-flash"]);
  assert.equal(updated["opencode-go"].models[1].baseUrl, "https://opencode.ai/zen/go/v1");
  assert.equal(updated["opencode-go"].models[1].provider, "opencode-go");
  assert.equal(updated.deepseek, existing.deepseek);
  assert.equal(updated["opencode-go"].lastModified, 2);
  assert.equal(supplementGoFlash(updated, 2), updated);
  assert.equal(goFlash.name, "DeepSeek V4.1 Flash");
});
