import { writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

// User's FeralUI preset. Dividers bound color regions; stops sit at their midpoints.
const colors = ["#080C3F", "#273782", "#6E7BCC", "#BCC4F1", "#E4E9FA"];
const boundaries = [0, 0.504, 0.734, 0.89, 0.95, 1];
const positions = colors.map((_, i) => (boundaries[i] + boundaries[i + 1]) / 2);

function toOklab(hex) {
	const [r, g, b] = hex.match(/[\da-f]{2}/gi).map((channel) => {
		const value = Number.parseInt(channel, 16) / 255;
		return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
	});
	const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
	const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
	const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
	return [
		0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
		1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
		0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
	];
}

function toSrgb([lightness, a, b]) {
	const l = (lightness + 0.3963377774 * a + 0.2158037573 * b) ** 3;
	const m = (lightness - 0.1055613458 * a - 0.0638541728 * b) ** 3;
	const s = (lightness - 0.0894841775 * a - 1.291485548 * b) ** 3;
	return [
		4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
		-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
		-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
	].map((value) => {
		const encoded = value <= 0.0031308 ? value * 12.92 : 1.055 * Math.max(0, value) ** (1 / 2.4) - 0.055;
		return Math.max(0, Math.min(255, Math.round(encoded * 255)));
	});
}

const lab = colors.map(toOklab);
const scanline = Buffer.alloc(1 + 256 * 3); // PNG filter 0, one RGB row.
for (let pixel = 0; pixel < 256; pixel++) {
	const position = pixel / 255;
	let segment = 0;
	while (segment < colors.length - 2 && position > positions[segment + 1]) segment++;
	const amount = Math.max(0, Math.min(1, (position - positions[segment]) / (positions[segment + 1] - positions[segment])));
	const rgb = toSrgb(lab[segment].map((value, channel) => value + (lab[segment + 1][channel] - value) * amount));
	scanline.set(rgb, 1 + pixel * 3);
}

function chunk(type, data) {
	const payload = Buffer.concat([Buffer.from(type), data]);
	let crc = 0xffffffff;
	for (const byte of payload) {
		crc ^= byte;
		for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
	}
	const length = Buffer.alloc(4);
	length.writeUInt32BE(data.length);
	const checksum = Buffer.alloc(4);
	checksum.writeUInt32BE((crc ^ 0xffffffff) >>> 0);
	return Buffer.concat([length, payload, checksum]);
}

const header = Buffer.alloc(13);
header.writeUInt32BE(256, 0);
header.writeUInt32BE(1, 4);
header[8] = 8;
header[9] = 2;
const png = Buffer.concat([
	Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
	chunk("IHDR", header),
	chunk("IDAT", deflateSync(scanline)),
	chunk("IEND", Buffer.alloc(0)),
]);
writeFileSync(new URL("./blue-hour-dark-palette.png", import.meta.url), png);
console.log(`Dark palette: ${positions.map((position) => `${(position * 100).toFixed(1)}%`).join(", ")}`);
