import { SettingsSection } from "../settings/settings-frame";

const shortcutGroups = [
	{
		title: "全局与窗口",
		items: [
			{ command: "打开命令面板与常见导航操作", keys: "Ctrl + P" },
			{ command: "快速打开指定会话与近期导出", keys: "Ctrl + O" },
			{ command: "新建对话（浏览器环境避让 Ctrl + N）", keys: "Alt + N" },
			{ command: "展开或收起左侧会话导航栏", keys: "Ctrl + B" },
			{ command: "打开设置页面", keys: "Ctrl + ," },
			{ command: "聚焦当前页面或弹窗的输入框", keys: "Ctrl + J" },
		],
	},
	{
		title: "对话与输入",
		items: [
			{ command: "发送当前消息（中文输入法候选态下仅选词）", keys: "Enter" },
			{ command: "在聊天输入框中换行", keys: "Shift + Enter" },
			{ command: "打开模型与思考强度选择器", keys: "Alt + M" },
			{ command: "唤出斜杠命令与诊断技能补全面板", keys: "行首 /" },
			{ command: "检索当前会话工作目录文件并附加引用", keys: "@" },
			{ command: "补全打开时接受候选项；补全关闭时移出输入焦点", keys: "Tab" },
			{ command: "方向键浏览列表与按钮；输入框首末行跳至起止", keys: "↑ ↓ ← →" },
			{ command: "逐级关闭浮层与补全、停止当前生成或退出输入态", keys: "Esc" },
		],
	},
	{
		title: "列表与排序",
		items: [{ command: "上下移动已启用模型或排队消息顺序", keys: "Alt + ↑ / ↓" }],
	},
] as const;

export default function ShortcutSettings() {
	return (
		<section
			className="settings-shortcuts-pane"
			aria-label="快捷键列表"
			ref={(node) => {
				if (node) node.tabIndex = -1;
			}}
		>
			{shortcutGroups.map((group) => (
				<SettingsSection title={group.title} key={group.title}>
					<div className="settings-flat-list settings-shortcuts">
						{group.items.map((shortcut) => (
							<div className="settings-flat-row" key={shortcut.keys}>
								<span>{shortcut.command}</span>
								<kbd className="settings-key-kbd">{shortcut.keys}</kbd>
							</div>
						))}
					</div>
				</SettingsSection>
			))}
		</section>
	);
}
