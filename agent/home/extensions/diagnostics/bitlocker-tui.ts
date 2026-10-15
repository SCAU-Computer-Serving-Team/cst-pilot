import type { ExtensionContext, Theme } from "@earendil-works/pi-coding-agent";
import { CURSOR_MARKER, Input, Key, matchesKey, stripTerminalSequences, Text } from "@earendil-works/pi-tui";
import type { BitLockerInteraction } from "./bitlocker-core.ts";
import { type BitLockerVolume, validateRecoveryPassword } from "./bitlocker-data.ts";

const WAIT_MS = 180_000;

/** Only the masked representation is rendered; the Input retains standard paste/edit behavior. */
export class RecoveryPasswordInput {
	private input = new Input();
	private show = false;
	private error = "";
	private settled = false;
	private timer: ReturnType<typeof setTimeout>;
	private abort = () => this.finish(undefined);

	private volume: BitLockerVolume;
	private theme: Theme;
	private redraw: () => void;
	private done: (value: string | undefined) => void;
	private signal?: AbortSignal;

	constructor(
		volume: BitLockerVolume,
		theme: Theme,
		redraw: () => void,
		done: (value: string | undefined) => void,
		signal?: AbortSignal,
	) {
		this.volume = volume;
		this.theme = theme;
		this.redraw = redraw;
		this.done = done;
		this.signal = signal;
		this.input.focused = true;
		this.input.onEscape = this.abort;
		this.input.onSubmit = (value) => {
			try {
				this.finish(validateRecoveryPassword(value));
			} catch {
				this.error = "请输入八组六位数字，用连字符分隔";
				this.redraw();
			}
		};
		this.timer = setTimeout(this.abort, WAIT_MS);
		signal?.addEventListener("abort", this.abort, { once: true });
		if (signal?.aborted) this.abort();
	}
	get focused(): boolean {
		return this.input.focused;
	}
	set focused(value: boolean) {
		this.input.focused = value;
	}
	private finish(value: string | undefined): void {
		if (this.settled) return;
		this.settled = true;
		this.dispose();
		this.done(value);
	}
	handleInput(data: string): void {
		if (this.settled) return;
		if (matchesKey(data, Key.ctrl("r"))) this.show = !this.show;
		else this.input.handleInput(data);
		this.redraw();
	}
	render(width: number): string[] {
		const ids = this.volume.keyProtectors
			.filter((key) => key.type === 3)
			.map((key) => key.id)
			.join("、");
		const header = new Text(
			[
				this.theme.fg("accent", `解锁 ${this.volume.drive ?? this.volume.volumeId}`),
				`恢复保护器 ID：${ids || "未获取"}`,
				"恢复密码直接交给本地 Windows 接口，不发送给模型。",
				"缺少密钥：https://aka.ms/myrecoverykey",
				"Enter 解锁 · Esc 取消 · Ctrl+R 显示/隐藏（显示内容可被终端录制）",
				this.error,
			]
				.filter(Boolean)
				.join("\n"),
			0,
			0,
		).render(width);
		const input = this.input.render(width).map((line) =>
			this.show
				? line
				: line
						.split(CURSOR_MARKER)
						.map((part) => stripTerminalSequences(part).replace(/\S/gu, "*"))
						.join(CURSOR_MARKER),
		);
		return [...header, ...input];
	}
	invalidate(): void {
		this.input.invalidate();
	}
	dispose(): void {
		clearTimeout(this.timer);
		this.signal?.removeEventListener("abort", this.abort);
		this.input = new Input();
		this.show = false;
	}
}

export function bitLockerInteraction(ctx: ExtensionContext): BitLockerInteraction {
	return {
		available: ctx.mode === "tui" && ctx.hasUI,
		password: (volume, signal) =>
			ctx.ui.custom<string | undefined>(
				(tui, theme, _keys, done) =>
					new RecoveryPasswordInput(volume, theme, () => tui.requestRender(), done, signal),
			),
		confirmParameter: (volume, signal) =>
			ctx.ui.confirm(
				`解锁 ${volume.drive ?? volume.volumeId}`,
				"将使用工具参数中的恢复密码。该路径的凭据可能已进入模型上下文并发送给模型服务商；确认后只解锁，不关闭加密。",
				{ signal, timeout: WAIT_MS },
			),
		confirmDecryption: (volumes, signal) =>
			ctx.ui.confirm(
				"确认关闭 BitLocker 并完整解密",
				[
					"以下为本次固定目标清单。解密会解除保护，完成后移除保护器；耗时取决于磁盘与负载。",
					...volumes.map(
						(v) =>
							`${v.drive ?? "无盘符"} · ${v.volumeId} · 转换状态 ${v.conversionStatus}${v.conversionStatus === 2 || v.conversionStatus === 4 ? "（将转为解密）" : ""}`,
					),
					"逐卷执行，部分失败不会自动回滚；不会自动清除数据卷自动解锁密钥。",
				].join("\n"),
				{ signal, timeout: WAIT_MS },
			),
	};
}
