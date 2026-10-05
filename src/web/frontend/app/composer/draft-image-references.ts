export const REFERENCE_PREFIX = "cst-draft-images:";
export const RELOAD_GRACE_MS = 60_000;
export type ImageReferences = { hashes: string[]; updatedAt: number };

export function parseImageHashes(value: string | null): string[] {
	if (value === null) return [];
	const parsed: unknown = JSON.parse(value);
	if (!Array.isArray(parsed) || !parsed.every((hash) => typeof hash === "string")) throw new Error("图片草稿记录损坏");
	return [...new Set(parsed)];
}

export function readDraftImageHashes(storage: Storage): string[] {
	const hashes = new Set<string>();
	for (let index = 0; index < storage.length; index++) {
		const key = storage.key(index);
		if (key?.startsWith("cst-draft:") && key.endsWith(":images"))
			for (const hash of parseImageHashes(storage.getItem(key))) hashes.add(hash);
	}
	return [...hashes];
}

export function retainedImageHashes(
	references: { owner: string; record: ImageReferences }[],
	liveOwners: Set<string>,
	now: number,
): { hashes: Set<string>; staleOwners: string[] } {
	const hashes = new Set<string>();
	const staleOwners: string[] = [];
	for (const { owner, record } of references) {
		if (liveOwners.has(owner) || now - record.updatedAt <= RELOAD_GRACE_MS) {
			for (const hash of record.hashes) hashes.add(hash);
		} else staleOwners.push(owner);
	}
	return { hashes, staleOwners };
}

export function parseImageReferences(value: string): ImageReferences {
	const parsed: unknown = JSON.parse(value);
	if (
		!parsed ||
		typeof parsed !== "object" ||
		!("hashes" in parsed) ||
		!("updatedAt" in parsed) ||
		typeof parsed.updatedAt !== "number" ||
		!Number.isFinite(parsed.updatedAt)
	)
		throw new Error("图片引用记录损坏");
	return { hashes: parseImageHashes(JSON.stringify(parsed.hashes)), updatedAt: parsed.updatedAt };
}
