import { type ExtensionAPI, getAgentDir } from "@earendil-works/pi-coding-agent";
import { catalogProviderFactories } from "./catalog-factories.ts";
import { cstoaProvider } from "./provider.ts";

export default function oauth(pi: ExtensionAPI): void {
	const home = getAgentDir();
	catalogProviderFactories(home).set("cstoa", () => cstoaProvider(home));
	let isTui = false;
	pi.on("session_start", (_event, ctx) => {
		isTui = ctx.mode === "tui";
	});
	pi.registerProvider(cstoaProvider(home, { isTui: () => isTui }));
}
