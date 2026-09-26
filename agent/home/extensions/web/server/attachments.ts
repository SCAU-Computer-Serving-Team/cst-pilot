import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";

export interface ImageRef {
	id: string;
	mimeType: string;
	bytes: number;
}
const formats = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
const MAX_IMAGE_BYTES = 12 * 1024 * 1024;

function signature(data: Buffer, mimeType: string): boolean {
	if (mimeType === "image/png") return data.subarray(0, 8).equals(Buffer.from("89504e470d0a1a0a", "hex"));
	if (mimeType === "image/jpeg") return data.subarray(0, 3).equals(Buffer.from("ffd8ff", "hex"));
	if (mimeType === "image/gif") return ["GIF87a", "GIF89a"].includes(data.toString("ascii", 0, 6));
	if (mimeType === "image/webp")
		return data.toString("ascii", 0, 4) === "RIFF" && data.toString("ascii", 8, 12) === "WEBP";
	return false;
}

export class ImageStore {
	private readonly root: string;
	constructor(root: string) {
		this.root = root;
	}

	async accept(sessionId: string, images: unknown): Promise<ImageRef[]> {
		if (!/^[a-zA-Z0-9_-]{1,128}$/.test(sessionId)) throw new Error("图片会话 ID 无效");
		if (!Array.isArray(images)) throw new Error("图片格式无效");
		const results: ImageRef[] = [];
		let remaining = MAX_IMAGE_BYTES;
		for (const image of images) {
			if (
				!image ||
				typeof image !== "object" ||
				typeof image.mimeType !== "string" ||
				!formats.has(image.mimeType) ||
				typeof image.data !== "string" ||
				image.data.length > Math.ceil(remaining / 3) * 4 + 4 ||
				!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(image.data)
			) {
				throw new Error("图片格式或体积无效");
			}
			const bytes = Buffer.from(image.data, "base64");
			if (!bytes.length || bytes.length > remaining || !signature(bytes, image.mimeType))
				throw new Error("图片格式或体积无效");
			remaining -= bytes.length;
			const id = createHash("sha256").update(bytes).digest("hex");
			const folder = join(this.root, sessionId);
			await mkdir(folder, { recursive: true });
			const file = join(folder, id);
			const temp = `${file}.${randomUUID()}.tmp`;
			try {
				await writeFile(temp, bytes, { flag: "wx" });
				await rename(temp, file);
			} catch (error) {
				await rm(temp, { force: true }).catch(() => undefined);
				throw error;
			}
			results.push({ id, mimeType: image.mimeType, bytes: bytes.length });
		}
		return results;
	}

	async read(sessionId: string, id: string): Promise<Buffer> {
		if (!/^[a-zA-Z0-9_-]{1,128}$/.test(sessionId) || !/^[0-9a-f]{64}$/.test(id)) throw new Error("图片 ID 无效");
		return readFile(join(this.root, sessionId, id));
	}
}
