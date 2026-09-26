import assert from "node:assert/strict";
import { test } from "node:test";
import { WebUiBridge } from "./ui.ts";

test("selection is visible, validates answer and resolves exactly once", async () => {
	const bridge = new WebUiBridge();
	const pending = bridge.context().select("Choose", ["A", "B"]);
	const [question] = bridge.snapshot();
	assert.equal(question.kind, "select");
	assert.throws(() => bridge.respond(question.requestId, "C"));
	bridge.respond(question.requestId, "B");
	assert.equal(await pending, "B");
	assert.deepEqual(bridge.snapshot(), []);
	assert.throws(() => bridge.respond(question.requestId, "A"));
});

test("input and multi-line editor preserve entered text", async () => {
	const bridge = new WebUiBridge();
	const input = bridge.context().input("Device name", "default");
	bridge.respond(bridge.snapshot()[0].requestId, "Laptop");
	assert.equal(await input, "Laptop");
	const editor = bridge.context().editor("Details", "draft");
	bridge.respond(bridge.snapshot()[0].requestId, "line 1\nline 2");
	assert.equal(await editor, "line 1\nline 2");
});

test("closing cancels unanswered confirmation", async () => {
	const bridge = new WebUiBridge();
	const answer = bridge.context().confirm("Confirm", "Continue?");
	bridge.close();
	assert.equal(await answer, false);
});
