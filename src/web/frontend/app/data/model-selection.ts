import type { Model } from "./web-state";

export type ModelChoice = Model & {
	providerName?: string;
	credentialReady: boolean;
	catalogPresent: boolean;
	thinkingLevel?: string;
};

export type ModelSelectionPayload = {
	models: Model[];
	catalog: ModelChoice[];
	defaultModel: ModelChoice;
	selected: ModelChoice | null;
	enabled: string[] | null;
	unavailableEnabled: string[];
	thinkingLevels?: string[];
};

/** 手动选择独立于默认值；目录和凭据更新只刷新所选模型的状态。 */
export function refreshManualSelection(current: ModelChoice | null, data: ModelSelectionPayload, manual: boolean) {
	if (!manual || !current) return data.selected;
	return (
		data.catalog.find((model) => model.provider === current.provider && model.id === current.id) ?? {
			...current,
			catalogPresent: false,
			credentialReady: false,
		}
	);
}
