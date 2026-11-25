import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import registerBitLocker from "./bitlocker.ts";
import registerDisk from "./disk.ts";
import registerDriver from "./driver.ts";
import registerEventlog from "./eventlog.ts";
import registerLs from "./ls.ts";
import registerRunbook from "./runbook.ts";
import registerStartup from "./startup.ts";
import registerSys from "./sys.ts";

/** One loader instance owns the diagnostic tools. */
export default function diagnostics(pi: ExtensionAPI): void {
	registerBitLocker(pi);
	registerDisk(pi);
	registerDriver(pi);
	registerEventlog(pi);
	registerLs(pi);
	registerRunbook(pi);
	registerStartup(pi);
	registerSys(pi);
}
