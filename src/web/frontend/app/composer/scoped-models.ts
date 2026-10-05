import type { Model } from "../data/web-state";

/** 保留 scoped 模型的顺序；当前模型不在范围内时仅显示名称，不把它加入可选项。 */
export function scopedModels(models: Model[], enabled: string[] | null): Model[] {
	if (enabled === null) return models;
	const byId = new Map(models.map((model) => [`${model.provider}/${model.id}`, model]));
	return enabled.flatMap((id) => {
		const model = byId.get(id);
		return model ? [model] : [];
	});
}
