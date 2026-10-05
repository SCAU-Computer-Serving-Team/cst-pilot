import assert from "node:assert/strict";
import { test } from "node:test";
import { IDBFactory } from "fake-indexeddb";
import { parseImageHashes, REFERENCE_PREFIX, RELOAD_GRACE_MS, readDraftImageHashes } from "./draft-image-references.ts";
import { addImage, collectDraftImages, loadImages, releaseImageHolds } from "./draft-images.ts";

class MemoryStorage implements Storage {
	private values = new Map<string, string>();
	get length() {
		return this.values.size;
	}
	clear() {
		this.values.clear();
	}
	getItem(key: string) {
		return this.values.get(key) ?? null;
	}
	key(index: number) {
		return [...this.values.keys()][index] ?? null;
	}
	removeItem(key: string) {
		this.values.delete(key);
	}
	setItem(key: string, value: string) {
		this.values.set(key, value);
	}
}

const local = new MemoryStorage();
const session = new MemoryStorage();
const held = new Set<string>();
let queued = Promise.resolve<unknown>(undefined);
const locks = {
	request(name: string, work: () => Promise<unknown>) {
		if (name.includes(":owner:")) {
			held.add(name);
			return work();
		}
		const result = queued.then(work);
		queued = result.catch(() => {});
		return result;
	},
	async query() {
		return { held: [...held].map((name) => ({ name })) };
	},
};
Object.defineProperties(globalThis, {
	indexedDB: { configurable: true, value: new IDBFactory() },
	localStorage: { configurable: true, value: local },
	sessionStorage: { configurable: true, value: session },
	navigator: { configurable: true, value: { locks } },
	window: { configurable: true, value: new EventTarget() },
});
const file = (text: string) => new File([text], "test.png", { type: "image/png" });
async function has(hash: string) {
	const images = await loadImages([hash]);
	for (const image of images) URL.revokeObjectURL(image.url);
	return images.length === 1;
}

function resetReferences() {
	local.clear();
	session.clear();
}

test("启动扫描包含本标签页所有会话，重复 hash 合并，损坏记录拒绝回收", () => {
	resetReferences();
	session.setItem("cst-draft:new:images", '["a", "a"]');
	session.setItem("cst-draft:s1:images", '["b"]');
	session.setItem("unrelated:images", '["c"]');
	assert.deepEqual(readDraftImageHashes(session), ["a", "b"]);
	assert.deepEqual(parseImageHashes(null), []);
	assert.throws(() => parseImageHashes("[1]"));
	session.setItem("cst-draft:s2:images", "{");
	assert.throws(() => readDraftImageHashes(session));
});

test("异步写入期间保留图片；提交草稿后保留；删除引用后回收字节", async () => {
	resetReferences();
	const image = await addImage(file("pending"), 0);
	await collectDraftImages();
	assert.equal(await has(image.hash), true);
	session.setItem("cst-draft:new:images", JSON.stringify([image.hash]));
	await releaseImageHolds([image]);
	assert.equal(await has(image.hash), true);
	session.removeItem("cst-draft:new:images");
	assert.equal(await collectDraftImages(), 1);
	assert.equal(await has(image.hash), false);
	URL.revokeObjectURL(image.url);
});

test("发送成功只清当前会话，切换页面后其他草稿仍可恢复", async () => {
	resetReferences();
	const a = await addImage(file("session-a"), 0);
	const b = await addImage(file("session-b"), 0);
	session.setItem("cst-draft:a:images", JSON.stringify([a.hash]));
	session.setItem("cst-draft:b:images", JSON.stringify([b.hash]));
	await releaseImageHolds([a, b]);
	session.removeItem("cst-draft:a:images");
	await collectDraftImages();
	assert.equal(await has(a.hash), false);
	assert.equal(await has(b.hash), true);
	URL.revokeObjectURL(a.url);
	URL.revokeObjectURL(b.url);
});

test("其他标签页即使长期未更新引用，存活锁仍保护共享图片", async () => {
	resetReferences();
	const image = await addImage(file("other-tab"), 0);
	local.setItem(`${REFERENCE_PREFIX}other`, JSON.stringify({ hashes: [image.hash], updatedAt: 0 }));
	held.add("cst-draft-images:owner:other");
	await releaseImageHolds([image]);
	assert.equal(await has(image.hash), true);
	held.delete("cst-draft-images:owner:other");
	await collectDraftImages();
	assert.equal(await has(image.hash), false);
	assert.equal(local.getItem(`${REFERENCE_PREFIX}other`), null);
	URL.revokeObjectURL(image.url);
});

test("关闭标签页保留刷新宽限，过期后回收；损坏引用时停止删除", async () => {
	resetReferences();
	const image = await addImage(file("reload"), 0);
	local.setItem(`${REFERENCE_PREFIX}previous`, JSON.stringify({ hashes: [image.hash], updatedAt: Date.now() }));
	await releaseImageHolds([image]);
	assert.equal(await has(image.hash), true);
	local.setItem(`${REFERENCE_PREFIX}broken`, "{");
	await assert.rejects(collectDraftImages);
	assert.equal(await has(image.hash), true);
	local.removeItem(`${REFERENCE_PREFIX}broken`);
	local.setItem(
		`${REFERENCE_PREFIX}previous`,
		JSON.stringify({ hashes: [image.hash], updatedAt: Date.now() - RELOAD_GRACE_MS - 1 }),
	);
	await collectDraftImages();
	assert.equal(await has(image.hash), false);
	URL.revokeObjectURL(image.url);
});

test("重复内容的并发写入分别持有引用，批次失败不清其他批次", async () => {
	resetReferences();
	const [a, b] = await Promise.all([addImage(file("duplicate"), 0), addImage(file("duplicate"), 0)]);
	assert.equal(a.hash, b.hash);
	await releaseImageHolds([a]);
	assert.equal(await has(b.hash), true);
	await releaseImageHolds([b]);
	assert.equal(await has(b.hash), false);
	URL.revokeObjectURL(a.url);
	URL.revokeObjectURL(b.url);
});

test("a live tab that has not published references yet blocks deletion", async () => {
	resetReferences();
	const image = await addImage(file("starting-tab"), 0);
	held.add("cst-draft-images:owner:starting");
	await releaseImageHolds([image]);
	assert.equal(await has(image.hash), true);
	held.delete("cst-draft-images:owner:starting");
	await collectDraftImages();
	assert.equal(await has(image.hash), false);
	URL.revokeObjectURL(image.url);
});

test("格式和预算错误不写入图片", async () => {
	await assert.rejects(addImage(new File(["bad"], "test.svg", { type: "image/svg+xml" }), 0), /仅支持/);
	await assert.rejects(addImage(file("too-large"), 12 * 1024 * 1024), /12 MiB/);
});
