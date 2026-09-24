import assert from "node:assert/strict";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "./http.ts";

const staticDirectory = fileURLToPath(new URL("../static/", import.meta.url));

async function availablePort(): Promise<number> {
	const probe = createNetServer();
	await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
	const { port } = probe.address() as AddressInfo;
	await new Promise<void>((resolve, reject) => probe.close((error) => (error ? reject(error) : resolve())));
	return port;
}

const port = await availablePort();
const server = createWebServer(staticDirectory, port);
await listenWebServer(server, port);
after(() => new Promise<void>((resolve, reject) => server.close((error) => (error ? reject(error) : resolve()))));

for (const pathname of ["/assets/missing", "/assets/missing.js", "/fonts/missing", "/assets", "/fonts"]) {
	test(`${pathname} is not an SPA navigation`, async () => {
		const response = await fetch(`http://127.0.0.1:${port}${pathname}`, { headers: { Accept: "text/html" } });
		assert.equal(response.status, 404);
		assert.match(response.headers.get("content-type") ?? "", /application\/json/);
	});
}

test("session navigation falls back to HTML", async () => {
	const response = await fetch(`http://127.0.0.1:${port}/s/example`, { headers: { Accept: "text/html" } });
	assert.equal(response.status, 200);
	assert.match(response.headers.get("content-type") ?? "", /text\/html/);
});
