import assert from "node:assert/strict";
import { test } from "node:test";
import type { BitLockerVolume } from "../../diagnostics/bitlocker-data.ts";
import { BitLockerWebInteraction } from "../../diagnostics/bitlocker-web.ts";

const volume: BitLockerVolume = {
	volumeId: "volume-test",
	drive: "D:",
	volumeType: 1,
	conversionStatus: 1,
	encryptionPercentage: 100,
	encryptionMethod: 7,
	protectionStatus: 1,
	lockStatus: 1,
	autoUnlockEnabled: false,
	autoUnlockKeysStored: null,
	keyProtectors: [{ id: "key-id", type: 3 }],
	collectionErrors: [],
};
const password = Array(8).fill("123456").join("-");

test("Web credentials use a single-use channel without entering snapshots", async () => {
	const bridge = new BitLockerWebInteraction();
	const pending = bridge.forCall("call-1").password(volume);
	const [request] = bridge.snapshot();
	assert.equal(request.toolCallId, "call-1");
	assert.equal(request.targets[0].keyProtectors[0].id, "key-id");
	assert.throws(() => bridge.respond(request.requestId, { password: "invalid" }));
	assert.equal(bridge.snapshot().length, 1);
	bridge.respond(request.requestId, { password });
	assert.equal(await pending, password);
	assert.deepEqual(bridge.snapshot(), []);
	assert.throws(() => bridge.respond(request.requestId, { password }));
});

test("confirmation targets are fixed snapshots; cancellation and abort remove requests", async () => {
	const bridge = new BitLockerWebInteraction();
	const targets = [structuredClone(volume)];
	const confirmation = bridge.forCall("call-2").confirmDecryption(targets);
	targets[0].drive = "E:";
	const [request] = bridge.snapshot();
	assert.equal(request.targets[0].drive, "D:");
	bridge.respond(request.requestId, { cancel: true });
	assert.equal(await confirmation, false);
	const abort = new AbortController();
	const input = bridge.forCall("call-3").password(volume, abort.signal);
	abort.abort();
	assert.equal(await input, undefined);
	assert.deepEqual(bridge.snapshot(), []);
});

test("expiry, session disposal and separate sessions do not revive requests", async () => {
	const bridge = new BitLockerWebInteraction(5);
	assert.equal(await bridge.forCall("call-4").password(volume), undefined);
	const first = new BitLockerWebInteraction();
	const second = new BitLockerWebInteraction();
	const input = first.forCall("same-id").password(volume);
	assert.throws(() => second.respond(first.snapshot()[0].requestId, { password }));
	first.close();
	assert.equal(await input, undefined);
	assert.deepEqual(first.snapshot(), []);
});
