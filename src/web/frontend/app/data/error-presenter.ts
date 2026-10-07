export function presentError(message: string) {
	const detail = message
		.replace(/(Bearer\s+)[^\s"',}]+/gi, "$1[已隐藏]")
		.replace(/((?:api[_-]?key|token|secret|password)["']?\s*[:=]\s*["']?)[^\s"',}]+/gi, "$1[已隐藏]")
		.slice(0, 4000);
	if (/\b401\b|unauthorized|invalid.api.key|authentication[_ .-]?(?:failed|error)|凭据.*(?:失效|过期)/i.test(message))
		return {
			kind: "auth",
			title: "模型服务需要重新登录",
			description: "当前服务拒绝了凭据。请重新登录后再发送消息。",
			action: "重新登录",
			detail,
		};
	if (/无法写入|空间不足|未能保存|ENOSPC|EACCES|EROFS|EPERM|storage_(full|unavailable)/i.test(message))
		return {
			kind: "storage",
			title: "操作未能保存",
			description: "请检查工具包是否可写或空间是否充足。未提交的内容会保留。",
			action: "重试",
			detail,
		};
	if (/连接.*断开|事件流.*断开|无法连接|Failed to fetch|network|fetch failed/i.test(message))
		return {
			kind: "connection",
			title: "与 CST Pilot 的连接已断开",
			description: "正在重连。请确认终端仍在运行；未发送内容会保留。",
			action: "重新连接",
			detail,
		};
	if (/\b429\b|rate.limit/i.test(message))
		return {
			kind: "rate",
			title: "模型服务请求过于频繁",
			description: "请稍后重试，或在设置中切换模型服务。",
			action: "模型设置",
			detail,
		};
	return {
		kind: "general",
		title: "操作未完成",
		description: "请检查当前设置后重试。技术详情可用于排查原因。",
		action: "模型设置",
		detail,
	};
}
