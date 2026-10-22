import { AsyncLocalStorage } from "node:async_hooks";
import {
	type AgentSession,
	estimateTokens,
	findCutPoint,
	type SettingsManager,
	sessionEntryToContextMessages,
} from "@earendil-works/pi-coding-agent";

type Settings = ReturnType<SettingsManager["getCompactionSettings"]>;
const scope = new AsyncLocalStorage<{ manager: SettingsManager; settings?: Settings }>();
const installed = new WeakSet<SettingsManager>();

/** Short manual requests may summarize older content even below Pi's recent-context budget. */
function manualSettings(session: AgentSession, settings: Settings): Settings {
	const entries = session.sessionManager.getBranch();
	let start = 0;
	for (let i = entries.length - 1; i >= 0; i--) {
		const entry = entries[i];
		if (entry.type !== "compaction") continue;
		if (entries.slice(i + 1).every((item) => sessionEntryToContextMessages(item).length === 0))
			throw new Error("Already compacted");
		const kept = entries.findIndex((item) => item.id === entry.firstKeptEntryId);
		start = kept >= 0 ? kept : i + 1;
		break;
	}
	const hasPrefix = (budget: number) => {
		const cut = findCutPoint(entries, start, entries.length, budget);
		return entries
			.slice(start, cut.firstKeptEntryIndex)
			.some((entry) => entry.type !== "compaction" && sessionEntryToContextMessages(entry).length > 0);
	};
	if (hasPrefix(settings.keepRecentTokens)) return settings;
	const tokens = entries
		.slice(start)
		.flatMap(sessionEntryToContextMessages)
		.reduce((sum, message) => sum + estimateTokens(message), 0);
	let keepRecentTokens = Math.min(settings.keepRecentTokens, Math.floor(tokens / 2));
	while (keepRecentTokens > 0 && !hasPrefix(keepRecentTokens)) keepRecentTokens = Math.floor(keepRecentTokens / 2);
	return { ...settings, keepRecentTokens };
}

export function compactSession(session: AgentSession, instructions?: string) {
	const manager = session.settingsManager;
	if (!installed.has(manager)) {
		const original = manager.getCompactionSettings.bind(manager);
		// Pi reads this once to prepare manual compaction. Consume it so event listeners and queued turns use normal settings.
		// Async-local reads avoid temporary shared overrides or disk writes.
		manager.getCompactionSettings = () => {
			const current = scope.getStore();
			if (current?.manager === manager && current.settings) {
				const settings = current.settings;
				current.settings = undefined;
				return settings;
			}
			return original();
		};
		installed.add(manager);
	}
	const settings = manualSettings(session, manager.getCompactionSettings());
	return scope.run({ manager, settings }, () => session.compact(instructions));
}
