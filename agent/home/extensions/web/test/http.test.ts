import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { after, test } from "node:test";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { testRoot } from "./support.ts";

const staticDirectory = await mkdtemp(join(await testRoot(), "cst-web-static-"));
await writeFile(join(staticDirectory, "index.html"), "<!doctype html><html><body>test</body></html>");

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
after(async () => {
	await new Promise<void>((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
	await rm(staticDirectory, { recursive: true, force: true });
});

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
