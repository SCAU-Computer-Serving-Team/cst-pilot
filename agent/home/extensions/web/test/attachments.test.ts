import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { ImageStore } from "../server/attachments.ts";
import { testRoot } from "./support.ts";

const rootDir = await testRoot();

test("the image budget and MIME signature are checked before acceptance", async () => {
	const root = await mkdtemp(join(rootDir, "cst-images-"));
	try {
		const images = new ImageStore(root);
		const header = Buffer.from("89504e470d0a1a0a", "hex");
		await assert.rejects(images.accept("session", [{ mimeType: "image/jpeg", data: header.toString("base64") }]));
		const huge = Buffer.concat([header, Buffer.alloc(12 * 1024 * 1024)]);
		await assert.rejects(images.accept("session", [{ mimeType: "image/png", data: huge.toString("base64") }]));
		await assert.rejects(images.accept("../auth", [{ mimeType: "image/png", data: header.toString("base64") }]));
		const [ref] = await images.accept("session", [{ mimeType: "image/png", data: header.toString("base64") }]);
		assert.equal((await images.read("session", ref.id)).toString("hex"), header.toString("hex"));
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});
