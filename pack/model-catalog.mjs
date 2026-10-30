// Offline fallback for the release default, matching the pi.dev provider catalog.
// Preserve upstream entries without changing auth.json or other providers.
export const deepseekFlash = {
  id: "deepseek-flash",
  name: "DeepSeek V4.1 Flash",
  api: "openai-completions",
  provider: "deepseek",
  baseUrl: "https://api.deepseek.com",
  reasoning: true,
  input: ["text", "image"],
  cost: { input: 0.3, output: 1.2, cacheRead: 0.006, cacheWrite: 0 },
  compat: {
    supportsStore: false,
    supportsDeveloperRole: false,
    supportsStrictMode: true,
    maxTokensField: "max_tokens",
    requiresReasoningContentOnAssistantMessages: true,
    thinkingFormat: "deepseek",
  },
  contextWindow: 1000000,
  maxTokens: 384000,
  thinkingLevelMap: { off: null, minimal: null, low: "low", medium: null, high: "high", xhigh: null, max: "max" },
  inputLimits: {
    images: { resize: { maxWidth: 2000, maxHeight: 2000, maxBytes: 4718592, jpegQuality: 80 } },
  },
  type: "chat",
};

export function supplementDeepSeekFlash(catalog, generatedAt = Date.now()) {
  const provider = catalog?.deepseek;
  if (!provider?.models || !Array.isArray(provider.models)) {
    throw new Error("离线模型目录缺少 deepseek，不能补全默认模型");
  }
  const present = provider.models.some((model) => model.id === deepseekFlash.id);
  if (present) return catalog;
  return {
    ...catalog,
    deepseek: {
      ...provider,
      // Pi discards cached overlays older than its bundled catalog.
      lastModified: Math.max(provider.lastModified ?? 0, generatedAt),
      models: [...provider.models, deepseekFlash],
    },
  };
}
