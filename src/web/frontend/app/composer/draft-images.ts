import {
	type ImageReferences,
	parseImageReferences,
	REFERENCE_PREFIX,
	readDraftImageHashes,
	retainedImageHashes,
} from "./draft-image-references.ts";

export type DraftImage = { hash: string; mimeType: string; blob: Blob; url: string };
const supported = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
const budget = 12 * 1024 * 1024;
const lockName = "cst-draft-images:storage";
const ownerLockPrefix = "cst-draft-images:owner:";
const pending = new Map<string, number>();
let owner: string | undefined;
let ownerReady: Promise<void> | undefined;

function ensureOwner(): Promise<void> {
	if (ownerReady) return ownerReady;
	owner = crypto.randomUUID();
	window.addEventListener("pagehide", () => {
		try {
			publishReferences();
		} catch {
			/* 保留已有引用，下一次启动再尝试回收。 */
		}
	});
	ownerReady = navigator.locks
		? new Promise<void>((resolve, reject) => {
				void navigator.locks
					.request(`${ownerLockPrefix}${owner}`, () => {
						resolve();
						return new Promise<void>(() => {});
					})
					.catch(reject);
			})
		: Promise.resolve();
	return ownerReady;
}

async function withImageLock<T>(work: () => Promise<T>): Promise<T> {
	await ensureOwner();
	return navigator.locks ? navigator.locks.request(lockName, work) : work();
}

function publishReferences() {
	localStorage.setItem(
		`${REFERENCE_PREFIX}${owner}`,
		JSON.stringify({
			hashes: [...new Set([...readDraftImageHashes(sessionStorage), ...pending.keys()])],
			updatedAt: Date.now(),
		}),
	);
}

function openDb(): Promise<IDBDatabase> {
	return new Promise((resolve, reject) => {
		const request = indexedDB.open("cst-web-drafts", 1);
		request.onupgradeneeded = () => request.result.createObjectStore("images");
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => reject(request.error);
	});
}

async function store(key: string, value?: Blob): Promise<Blob | undefined> {
	const db = await openDb();
	try {
		return await new Promise<Blob | undefined>((resolve, reject) => {
			const tx = db.transaction("images", value ? "readwrite" : "readonly");
			const request = value ? tx.objectStore("images").put(value, key) : tx.objectStore("images").get(key);
			let result: Blob | undefined;
			request.onsuccess = () => {
				result = value ?? request.result;
			};
			tx.oncomplete = () => resolve(result);
			tx.onerror = tx.onabort = () => reject(tx.error ?? new Error("图片存储失败"));
		});
	} finally {
		db.close();
	}
}

/** 调用者先同步 sessionStorage，再发布全部会话的引用；缺少跨页锁时只发布，不回收。 */
export async function collectDraftImages(): Promise<number> {
	return withImageLock(async () => {
		publishReferences();
		if (!navigator.locks) return 0;
		const live = await navigator.locks.query();
		const liveOwners = new Set(
			live.held?.flatMap((lock) =>
				lock.name?.startsWith(ownerLockPrefix) ? [lock.name.slice(ownerLockPrefix.length)] : [],
			),
		);
		const references: { owner: string; record: ImageReferences }[] = [];
		for (let index = 0; index < localStorage.length; index++) {
			const key = localStorage.key(index);
			if (!key?.startsWith(REFERENCE_PREFIX)) continue;
			const value = localStorage.getItem(key);
			if (value !== null)
				references.push({ owner: key.slice(REFERENCE_PREFIX.length), record: parseImageReferences(value) });
		}
		if ([...liveOwners].some((liveOwner) => !references.some((reference) => reference.owner === liveOwner))) return 0;
		const retained = retainedImageHashes(references, liveOwners, Date.now());
		const db = await openDb();
		let deleted = 0;
		try {
			await new Promise<void>((resolve, reject) => {
				const tx = db.transaction("images", "readwrite");
				const cursor = tx.objectStore("images").openCursor();
				cursor.onsuccess = () => {
					const row = cursor.result;
					if (!row) return;
					if (typeof row.key === "string" && !retained.hashes.has(row.key)) {
						row.delete();
						deleted++;
					}
					row.continue();
				};
				tx.oncomplete = () => resolve();
				tx.onerror = tx.onabort = () => reject(tx.error ?? new Error("图片回收失败"));
			});
			for (const stale of retained.staleOwners) localStorage.removeItem(`${REFERENCE_PREFIX}${stale}`);
			return deleted;
		} finally {
			db.close();
		}
	});
}

export async function releaseImageHolds(images: DraftImage[]): Promise<void> {
	await withImageLock(async () => {
		for (const { hash } of images) {
			const count = pending.get(hash) ?? 0;
			if (count <= 1) pending.delete(hash);
			else pending.set(hash, count - 1);
		}
		publishReferences();
	});
	await collectDraftImages();
}

export async function addImage(file: File, currentBytes: number): Promise<DraftImage> {
	if (!supported.has(file.type)) throw new Error("仅支持 PNG、JPEG、GIF、WebP 图片");
	if (file.size + currentBytes > budget) throw new Error("图片总大小不能超过 12 MiB");
	const bytes = await file.arrayBuffer();
	const hash = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), (value) =>
		value.toString(16).padStart(2, "0"),
	).join("");
	await withImageLock(async () => {
		pending.set(hash, (pending.get(hash) ?? 0) + 1);
		try {
			publishReferences();
			await store(hash, file);
		} catch (error) {
			const count = pending.get(hash) ?? 1;
			if (count === 1) pending.delete(hash);
			else pending.set(hash, count - 1);
			throw error;
		}
	});
	return { hash, blob: file, mimeType: file.type, url: URL.createObjectURL(file) };
}

export async function loadImages(hashes: string[]): Promise<DraftImage[]> {
	const images: DraftImage[] = [];
	let remaining = budget;
	try {
		for (const hash of hashes) {
			const blob = await store(hash);
			if (!blob || !supported.has(blob.type) || blob.size > remaining) continue;
			remaining -= blob.size;
			images.push({ hash, blob, mimeType: blob.type, url: URL.createObjectURL(blob) });
		}
		return images;
	} catch (error) {
		images.forEach((image) => {
			URL.revokeObjectURL(image.url);
		});
		throw error;
	}
}

export async function encodeImages(images: DraftImage[]) {
	return Promise.all(
		images.map(async (image) => {
			const bytes = new Uint8Array(await image.blob.arrayBuffer());
			let data = "";
			for (let index = 0; index < bytes.length; index += 8192)
				data += String.fromCharCode(...bytes.subarray(index, index + 8192));
			return { mimeType: image.mimeType, data: btoa(data) };
		}),
	);
}
