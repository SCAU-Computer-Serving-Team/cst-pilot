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

for (const pathname of ["/%", "/%E0%A4%A", "/api/%ZZ"]) {
	test(`malformed URI ${pathname} returns a structured error and leaves the server available`, async () => {
		const response = await fetch(`http://127.0.0.1:${port}${pathname}`, {
			signal: AbortSignal.timeout(2000),
		});
		assert.equal(response.status, 400);
		assert.equal((await response.json()).error.code, "invalid_path");
		const healthy = await fetch(`http://127.0.0.1:${port}/api/state`);
		assert.equal(healthy.status, 200);
	});
}

test("startup snapshot reads the kit release version", async () => {
	const previousHome = process.env.PI_CODING_AGENT_DIR;
	try {
		process.env.PI_CODING_AGENT_DIR = join(staticDirectory, "agent", "home");
		await writeFile(join(staticDirectory, "VERSION"), "cst-pilot v0.5\npi 0.85.1\n");
		const response = await fetch(`http://127.0.0.1:${port}/api/state`);
		assert.equal((await response.json()).version, "v0.5");
	} finally {
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
	}
});

test("startup snapshot offers the BitLocker skill", async () => {
	const response = await fetch(`http://127.0.0.1:${port}/api/state`);
	assert.equal(response.status, 200);
	assert.ok((await response.json()).commands.includes("/skill:bitlocker"));
});

test("session navigation falls back to HTML", async () => {
	const response = await fetch(`http://127.0.0.1:${port}/s/example`, { headers: { Accept: "text/html" } });
	assert.equal(response.status, 200);
	assert.match(response.headers.get("content-type") ?? "", /text\/html/);
});
