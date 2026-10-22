export function presentError(
	message: string,
	{ model = false, input = false }: { model?: boolean; input?: boolean } = {},
) {
	const detail = message
		.replace(/(Bearer\s+)[^\s"',}]+/gi, "$1[已隐藏]")
		.replace(/((?:api[_-]?key|token|secret|password)["']?\s*[:=]\s*["']?)[^\s"',}]+/gi, "$1[已隐藏]")
		.slice(0, 4000);
	if (/already_compacted|会话已压缩/.test(message))
		return {
			kind: "compaction",
			title: "会话已压缩",
			description: "上次压缩后没有新增对话，本次无需重复生成摘要。继续对话后可再次压缩。",
			action: "",
			detail,
		};
	if (/nothing_to_compact|当前没有可压缩的较早内容/.test(message))
		return {
			kind: "compaction",
			title: "暂无可压缩内容",
			description:
				"压缩会把较早的对话整理成摘要，并保留近期原文。当前内容不足以分出可总结的历史部分，请继续对话后再试。",
			action: "",
			detail,
		};
	if (/当前会话无需压缩/.test(message))
		return {
			kind: "compaction",
			title: "暂时无法压缩",
			description: "本次未找到可压缩的历史内容。可能是内容较少，或已压缩后没有新增对话；继续对话后可再次尝试。",
			action: "",
			detail,
		};
	if (/会话正在执行/.test(message))
		return { kind: "busy", title: "会话正在执行", description: "请等待当前操作完成后重试。", action: "", detail };
	if (/session_owned|该会话正由另一端使用/.test(message))
		return {
			kind: "owner",
			title: "会话正由另一端使用",
			description: "请先关闭持有此会话的 TUI 或 Web 服务，再重新打开。",
			action: "重试",
			detail,
		};
	if (/configuration_unavailable|(?:设置文件|模型或凭据配置|Web外观配置)无法读取/.test(message)) {
		const files = [...new Set(message.match(/(?:web-settings|settings|models|auth)\.json/g) ?? [])];
		return {
			kind: "configuration",
			title: "配置文件需要修正",
			description: `请检查${files.length ? ` ${files.join("、")} ` : "工具包配置文件"}，修正后重试。`,
			action: "重试",
			detail,
		};
	}
	if (/\b401\b|unauthorized|invalid.api.key|authentication[_ .-]?(?:failed|error)|凭据.*(?:失效|过期)/i.test(message))
		return {
			kind: "auth",
			title: "登录已失效",
			description: "",
			action: "重新登录",
			detail,
		};
	if (/无法写入|空间不足|未能保存|ENOSPC|EACCES|EROFS|EPERM|storage_(full|unavailable)/i.test(message))
		return {
			kind: "storage",
			title: "保存失败",
			description: input ? "输入已保留，请检查空间或权限。" : "请检查空间或权限。",
			action: "重试",
			detail,
		};
	if (/连接.*断开|事件流.*断开|无法连接|Failed to fetch|network|fetch failed/i.test(message))
		return {
			kind: "connection",
			title: input ? "提交待确认" : "连接断开",
			description: input ? "输入已保留，恢复后重试核查提交。" : "正在重连，输入已保留。",
			action: "重新连接",
			detail,
		};
	if (/\b429\b|rate.limit/i.test(message))
		return {
			kind: "rate",
			title: "请求过于频繁",
			description: "请稍后重试或切换模型。",
			action: "模型设置",
			detail,
		};
	return {
		kind: "general",
		title: model ? "请求失败" : "操作未完成",
		description: model ? "消息已保存。" : "请检查设置后重试。",
		action: "模型设置",
		detail,
	};
}
