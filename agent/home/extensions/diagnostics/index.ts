import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import registerDisk from "./disk.ts";
import registerDriver from "./driver.ts";
import registerEventlog from "./eventlog.ts";
import registerLs from "./ls.ts";
import registerPython from "./python.ts";
import registerRunbook from "./runbook.ts";
import registerStartup from "./startup.ts";
import registerSys from "./sys.ts";
import { createVolumeIndex } from "./wz-index.ts";

/** One loader instance owns all registered tools and their shared in-memory index. */
export default function diagnostics(pi: ExtensionAPI): void {
	const volumeIndex = createVolumeIndex();
	registerDisk(pi, volumeIndex);
	registerDriver(pi);
	registerEventlog(pi);
	registerLs(pi, volumeIndex);
	registerPython(pi);
	registerRunbook(pi);
	registerStartup(pi);
	registerSys(pi);
}
