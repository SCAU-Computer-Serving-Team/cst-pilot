import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:https";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { testRoot } from "../../web/test/support.ts";
import { sendBatch } from "../transport.ts";

const execute = promisify(execFile);
const openssl =
	process.env.CST_TELEMETRY_OPENSSL ??
	["C:/Program Files/Git/usr/bin/openssl.exe", "C:/Program Files/Git/mingw64/bin/openssl.exe"].find(existsSync) ??
	"openssl";

// 过期证书用 `openssl ca -startdate/-enddate` 签发。
// `openssl x509 -days -1` 在 OpenSSL 3.4 起被拒（Error: end date before start date），
// 不能跨版本使用；startdate/enddate 在所有支持版本上行为一致。
async function issueExpiredCert(dir: string, options: { env: NodeJS.ProcessEnv }) {
	const toPosix = (path: string) => path.replace(/\\/g, "/");
	const caConfig = join(dir, "ca.cnf");
	await writeFile(
		caConfig,
		[
			"[req]",
			"distinguished_name=dn",
			"[dn]",
			"[ca]",
			"default_ca=CA_default",
			"[CA_default]",
			`dir=${toPosix(dir)}`,
			`database=${toPosix(join(dir, "index.txt"))}`,
			`new_certs_dir=${toPosix(join(dir, "newcerts"))}`,
			`serial=${toPosix(join(dir, "serial"))}`,
			"default_md=sha256",
			"policy=policy_any",
			"unique_subject=no",
			"copy_extensions=copy",
			"[policy_any]",
			"commonName=supplied",
			"",
		].join("\n"),
	);
	await mkdir(join(dir, "newcerts"), { recursive: true });
	await writeFile(join(dir, "index.txt"), "");
	await writeFile(join(dir, "serial"), "1000\n");
	await execute(
		openssl,
		[
			"req",
			"-new",
			"-key",
			join(dir, "good.key"),
			"-out",
			join(dir, "expired.csr"),
			"-subj",
			"/CN=Telemetry test fixture",
			"-addext",
			"subjectAltName=IP:127.0.0.1",
			"-addext",
			"basicConstraints=critical,CA:TRUE",
		],
		options,
	);
	await execute(
		openssl,
		[
			"ca",
			"-batch",
			"-config",
			caConfig,
			"-cert",
			join(dir, "good.crt"),
			"-keyfile",
			join(dir, "good.key"),
			"-in",
			join(dir, "expired.csr"),
			"-out",
			join(dir, "expired.crt"),
			"-startdate",
			"20200101000000Z",
			"-enddate",
			"20200201000000Z",
			"-notext",
		],
		options,
	);
}

test("专用 CA 仅信任对应证书，错误主机名与过期证书不发送令牌", async () => {
	const dir = await mkdtemp(join(await testRoot(), "cst-telemetry-tls-"));
	try {
		const config = join(dir, "openssl.cnf");
		await writeFile(config, "[req]\ndistinguished_name=dn\n[dn]\n");
		const options = { env: { ...process.env, OPENSSL_CONF: config } };
		for (const [name, address] of [
			["good", "127.0.0.1"],
			["wrong-host", "192.0.2.1"],
		]) {
			await execute(
				openssl,
				[
					"req",
					"-x509",
					"-newkey",
					"rsa:2048",
					"-nodes",
					"-sha256",
					"-days",
					"1",
					"-keyout",
					join(dir, `${name}.key`),
					"-out",
					join(dir, `${name}.crt`),
					"-subj",
					"/CN=Telemetry test fixture",
					"-addext",
					`subjectAltName=IP:${address}`,
					"-addext",
					"basicConstraints=critical,CA:TRUE",
				],
				options,
			);
		}
		await issueExpiredCert(dir, options);
		for (const name of ["good", "wrong-host", "expired"]) {
			const cert = await readFile(join(dir, `${name}.crt`), "utf8");
			const key = await readFile(join(dir, `${name === "expired" ? "good" : name}.key`), "utf8");
			let requests = 0;
			const server = createServer({ cert, key }, (request, response) => {
				requests++;
				assert.equal(request.headers.authorization, "Bearer test-private-token");
				request.resume();
				response.writeHead(202);
				response.end("{}");
			});
			await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
			const address = server.address();
			assert.ok(address && typeof address !== "string");
			const endpoint = `https://127.0.0.1:${address.port}/v1/sessions`;
			try {
				assert.equal((await sendBatch(endpoint, "test-private-token", [])).kind, "retry");
				assert.equal(requests, 0);
				const result = await sendBatch(endpoint, "test-private-token", [], cert);
				assert.equal(result.kind, name === "good" ? "accepted" : "retry");
				assert.equal(requests, name === "good" ? 1 : 0);
			} finally {
				server.closeAllConnections();
				await new Promise<void>((resolve) => server.close(() => resolve()));
			}
		}
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});
