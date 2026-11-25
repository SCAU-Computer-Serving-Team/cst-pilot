import { SettingsHeading, SettingsSection } from "../settings/settings-frame";

const shortcutGroups = [
	{
		title: "全局与页面",
		items: [
			{ command: "打开命令面板与常见导航操作", keys: "Ctrl + P" },
			{ command: "快速打开指定会话与近期导出", keys: "Ctrl + O" },
			{ command: "新建对话（浏览器环境避让 Ctrl + N）", keys: "Alt + N" },
			{ command: "展开或收起左侧会话导航栏", keys: "Ctrl + B" },
			{ command: "打开设置页面", keys: "Ctrl + ," },
		],
	},
	{
		title: "对话与输入",
		items: [
			{ command: "打开命令补全", keys: "/" },
			{ command: "打开文件引用补全", keys: "@" },
			{ command: "补全命令", keys: "Tab" },
			{ command: "在补全面板中移动选项", keys: "↑ ↓" },
			{ command: "选中补全项；发送消息", keys: "Enter" },
			{ command: "在输入框中换行", keys: "Shift + Enter" },
			{ command: "逐级关闭悬浮面板；停止当前会话的执行", keys: "Esc" },
		],
	},
	{
		title: "模型偏好",
		items: [{ command: "调整启用模型顺序（聚焦排序手柄）", keys: "Alt + ↑ / ↓" }],
	},
] as const;

export default function ShortcutSettings() {
	return (
		<>
			<SettingsHeading title="快捷键" />
			{shortcutGroups.map((group) => (
				<SettingsSection title={group.title} key={group.title}>
					<div className="settings-card settings-shortcuts">
						{group.items.map((shortcut) => (
							<div className="settings-flat-row" key={shortcut.keys}>
								<span>{shortcut.command}</span>
								<kbd className="settings-key-kbd">{shortcut.keys}</kbd>
							</div>
						))}
					</div>
				</SettingsSection>
			))}
		</>
	);
}
