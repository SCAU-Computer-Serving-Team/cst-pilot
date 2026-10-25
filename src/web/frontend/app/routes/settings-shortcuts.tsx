import { Kbd } from "@heroui/react";
import { SettingsHeading, SettingsSection } from "../settings/settings-frame";

const shortcuts = [
	{ command: "打开命令补全", keys: "/" },
	{ command: "打开文件引用补全", keys: "@" },
	{ command: "补全命令", keys: "Tab" },
	{ command: "在补全面板中移动选项", keys: "↑ ↓" },
	{ command: "选中补全项；发送消息", keys: "Enter" },
	{ command: "在输入框中换行", keys: "Shift + Enter" },
	{ command: "关闭补全面板；停止当前会话的执行", keys: "Esc" },
];
export default function ShortcutSettings() {
	return (
		<>
			<SettingsHeading title="快捷键" />
			<SettingsSection title="聊天操作">
				<div className="settings-card settings-shortcuts">
					{shortcuts.map((shortcut) => (
						<div className="settings-flat-row" key={shortcut.keys}>
							<span>{shortcut.command}</span>
							<Kbd className="settings-key-kbd">{shortcut.keys}</Kbd>
						</div>
					))}
				</div>
			</SettingsSection>
		</>
	);
}
