import assert from "node:assert/strict";
import test from "node:test";

import { RELEASE_SETTINGS, RELEASE_TELEMETRY } from "../release-settings.mjs";

test("发行遥测同时指向 OA 与 Tim 接收端，并复用 OAuth 凭据", () => {
	assert.equal(RELEASE_SETTINGS.enableInstallTelemetry, false);
	assert.deepEqual(RELEASE_TELEMETRY, {
		enabled: true,
		endpoints: [
			{ url: "https://www.cstoa.top/api/telemetry" },
			{ url: "https://8.163.28.9:8445/api/telemetry", caFile: "timserver_1.crt" },
		],
		authProvider: "cstoa",
	});
});
