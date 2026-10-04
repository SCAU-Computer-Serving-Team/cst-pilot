import assert from "node:assert/strict";
import test from "node:test";

import { RELEASE_SETTINGS, RELEASE_TELEMETRY } from "../release-settings.mjs";

test("发行遥测指向 OA 接收端并复用 OAuth 凭据", () => {
	assert.equal(RELEASE_SETTINGS.enableInstallTelemetry, false);
	assert.deepEqual(RELEASE_TELEMETRY, {
		enabled: true,
		endpoint: "https://www.cstoa.top/api/telemetry",
		authProvider: "cstoa",
	});
});
