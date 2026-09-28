export type DraftImage = { hash: string; mimeType: string; blob: Blob; url: string };
const supported = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
const budget = 12 * 1024 * 1024;

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
      request.onsuccess = () => resolve(value ?? request.result as Blob | undefined);
      request.onerror = () => reject(request.error);
    });
  } finally { db.close(); }
}
export async function addImage(file: File, currentBytes: number): Promise<DraftImage> {
  if (!supported.has(file.type)) throw new Error("仅支持 PNG、JPEG、GIF、WebP 图片");
  if (file.size + currentBytes > budget) throw new Error("图片总大小不能超过 12 MiB");
  const bytes = await file.arrayBuffer();
  const hash = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), (value) => value.toString(16).padStart(2, "0")).join("");
  await store(hash, file);
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
    images.forEach((image) => URL.revokeObjectURL(image.url));
    throw error;
  }
}
export async function encodeImages(images: DraftImage[]) {
  return Promise.all(images.map(async (image) => {
    const bytes = new Uint8Array(await image.blob.arrayBuffer());
    let data = "";
    for (let index = 0; index < bytes.length; index += 8192) data += String.fromCharCode(...bytes.subarray(index, index + 8192));
    return { mimeType: image.mimeType, data: btoa(data) };
  }));
}
