import { type ExtensionAPI, getAgentDir } from "@earendil-works/pi-coding-agent";
import { cstoaProvider } from "./provider.ts";

export default function oauth(pi: ExtensionAPI): void {
	pi.registerProvider(cstoaProvider(getAgentDir()));
}
