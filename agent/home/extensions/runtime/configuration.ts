import type { ModelRuntime, SettingsManager } from "@earendil-works/pi-coding-agent";

export class ConfigurationUnavailable extends Error {
	readonly code = "CST_CONFIGURATION_UNAVAILABLE";
}
/** 对外只报告文件类别，Pi诊断中的原始配置值不进入HTTP或日志。 */
export function checkSharedConfiguration(settings: SettingsManager, models: ModelRuntime): void {
	if (settings.drainErrors().length)
		throw new ConfigurationUnavailable("设置文件无法读取，请检查 settings.json 后重试。");
	if (models.getError())
		throw new ConfigurationUnavailable("模型或凭据配置无法读取，请检查 models.json 和 auth.json 后重试。");
}
