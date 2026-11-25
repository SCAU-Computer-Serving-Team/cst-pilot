import type { Model } from "../data/web-state";

export const PICKER_MIN_WIDTH = 224;
export const PICKER_MAX_WIDTH = 320;
export const PICKER_ROW_HEIGHT = 32;
export const PICKER_ROW_GAP = 2;
export const PICKER_DIALOG_PADDING_V = 8;

const PICKER_DIALOG_PADDING_H = 8;
const PICKER_ROW_PADDING_H = 16;
const PICKER_SAFETY_H = 6;
const PICKER_MAIN_GAP = 12;
const PICKER_VALUE_GAP = 4;
const PICKER_CHEVRON_WIDTH = 14;
const PICKER_OPTION_GAP = 12;
const PICKER_META_GAP = 4;
const PICKER_CHECK_WIDTH = 14;
const PICKER_MAX_MODEL_VALUE_WIDTH = 180;
const PICKER_MAX_PROVIDER_WIDTH = 112;

export type ModelPickerPage = "main" | "models" | "thinking";

let measureContext: CanvasRenderingContext2D | null | undefined;

function getMeasureContext(): CanvasRenderingContext2D | null {
	if (measureContext !== undefined) return measureContext;
	if (typeof document === "undefined") {
		measureContext = null;
		return null;
	}
	try {
		measureContext = document.createElement("canvas").getContext("2d");
	} catch {
		measureContext = null;
	}
	return measureContext;
}

export function estimateTextWidth(text: string, fontSize: number, fontWeight = 400): number {
	if (!text) return 0;
	const ctx = getMeasureContext();
	if (ctx) {
		ctx.font = `${fontWeight} ${fontSize}px "Source Han Sans CN", sans-serif`;
		return Math.ceil(ctx.measureText(text).width);
	}
	let total = 0;
	for (const ch of text) {
		total += ch.charCodeAt(0) < 128 ? fontSize * 0.6 : fontSize * 1.04;
	}
	return Math.ceil(total);
}

export function modelDisplayName(model: Pick<Model, "id" | "name">): string {
	const trimmed = model.name.trim();
	return trimmed || model.id;
}

export function computeModelPickerWidth({
	page,
	selectedLabel,
	thinkingLabel,
	models,
	thinkingLabels,
}: {
	page: ModelPickerPage;
	selectedLabel: string;
	thinkingLabel: string;
	models: Pick<Model, "id" | "name" | "provider">[];
	thinkingLabels: string[];
}): number {
	const baseChrome = PICKER_DIALOG_PADDING_H + PICKER_ROW_PADDING_H + PICKER_SAFETY_H;
	let rawWidth = PICKER_MIN_WIDTH;

	if (page === "main") {
		const modelKeyW = estimateTextWidth("模型", 14, 400);
		const modelValW = Math.min(estimateTextWidth(selectedLabel, 12, 400), PICKER_MAX_MODEL_VALUE_WIDTH);
		const thinkKeyW = estimateTextWidth("思考强度", 14, 400);
		const thinkValW = estimateTextWidth(thinkingLabel, 12, 400);
		const rowChrome = baseChrome + PICKER_MAIN_GAP + PICKER_VALUE_GAP + PICKER_CHEVRON_WIDTH;
		rawWidth = Math.max(modelKeyW + modelValW, thinkKeyW + thinkValW) + rowChrome;
	} else if (page === "models") {
		const rowChrome = baseChrome + PICKER_OPTION_GAP + PICKER_META_GAP + PICKER_CHECK_WIDTH;
		let widest = PICKER_MIN_WIDTH;
		for (const model of models) {
			const labelW = estimateTextWidth(modelDisplayName(model), 13, 500);
			const providerW = Math.min(estimateTextWidth(model.provider, 11, 400), PICKER_MAX_PROVIDER_WIDTH);
			widest = Math.max(widest, labelW + providerW + rowChrome);
		}
		rawWidth = widest;
	} else {
		const rowChrome = baseChrome + PICKER_OPTION_GAP + PICKER_CHECK_WIDTH;
		let widest = PICKER_MIN_WIDTH;
		for (const label of thinkingLabels) {
			widest = Math.max(widest, estimateTextWidth(label, 13, 500) + rowChrome);
		}
		rawWidth = widest;
	}

	return Math.max(PICKER_MIN_WIDTH, Math.min(PICKER_MAX_WIDTH, Math.ceil(rawWidth)));
}

export function computeModelPickerHeight(page: ModelPickerPage, optionCount: number): number {
	const rowCount = page === "main" ? 2 : 1 + Math.max(1, optionCount);
	return rowCount * PICKER_ROW_HEIGHT + Math.max(0, rowCount - 1) * PICKER_ROW_GAP + PICKER_DIALOG_PADDING_V;
}
