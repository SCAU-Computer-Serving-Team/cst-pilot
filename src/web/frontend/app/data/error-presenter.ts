export function presentError(
	message: string,
	{ model = false, input = false }: { model?: boolean; input?: boolean } = {},
) {
	const detail = message
		.replace(/(Bearer\s+)[^\s"',}]+/gi, "$1[已隐藏]")
		.replace(/((?:api[_-]?key|token|secret|password)["']?\s*[:=]\s*["']?)[^\s"',}]+/gi, "$1[已隐藏]")
		.slice(0, 4000);
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
