// The bundled Pi catalog predates the OpenCode Go DeepSeek V4.1 Flash ID.
// Supplement the offline catalog without changing auth.json or other providers.
export const goFlash = {
  id: "deepseek-flash",
  name: "DeepSeek V4.1 Flash",
  api: "openai-completions",
  provider: "opencode-go",
  baseUrl: "https://opencode.ai/zen/go/v1",
  reasoning: true,
  input: ["text", "image"],
  // OpenCode Go has peak/off-peak pricing; use peak as an upper bound.
  cost: { input: 0.30, output: 1.20, cacheRead: 0.006, cacheWrite: 0 },
  compat: {
    supportsStore: false,
    supportsDeveloperRole: false,
    maxTokensField: "max_tokens",
    requiresReasoningContentOnAssistantMessages: true,
    thinkingFormat: "deepseek",
  },
  contextWindow: 1000000,
  maxTokens: 384000,
  thinkingLevelMap: { minimal: null, low: "low", medium: null, high: "high", max: "max" },
};

export function supplementGoFlash(catalog, generatedAt = Date.now()) {
  const provider = catalog?.["opencode-go"];
  if (!provider?.models || !Array.isArray(provider.models)) {
    throw new Error("离线模型目录缺少 opencode-go，不能补全默认模型");
  }
  const present = provider.models.some((model) => model.id === goFlash.id);
  if (present && (provider.lastModified ?? 0) >= generatedAt) return catalog;
  return {
    ...catalog,
    "opencode-go": {
      ...provider,
      // Pi discards cached overlays older than its bundled catalog.
      lastModified: Math.max(provider.lastModified ?? 0, generatedAt),
      models: present ? provider.models : [...provider.models, goFlash],
    },
  };
}
