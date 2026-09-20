import { StringEnum } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { runScope, SCOPES } from "./driver-core.ts";
import { diagnosticResult, OUTPUT_GUIDELINE, throwOnError } from "./result.ts";

/**
 * driver - 只读设备与驱动健康工具（cst-pilot 定制）
 *
 * 结构只读：WMI/CIM 纯查询，不装驱动、不启用禁用设备、不改服务。
 * 排查蓝牙 / 网卡 / 声卡等设备的驱动有没有出问题、出在哪个设备上，
 * 以及外接设备认没认出来。单工具多 scope（sys 同构）。
 *
 * 设计：doc/design/tool/driver-design.md。
 * 全部逻辑（CMD 构造 / 白名单校验 / 采集收敛 / scope 路由）在
 * driver-core.ts（不依赖 pi 注册层）；本文件只做注册薄壳。
 *
 * scope 分支：
 *   problem   异常设备排查（默认）。Status=Error/Unknown 的 PnP 设备，
 *             附硬件 ID 与原始错误码
 *   core      Net / Bluetooth / Audio / 显示四类硬件现状快照，
 *             附 bthserv / Audiosrv 服务状态与驱动版本日期
 *   external  外置设备排查：USB / 蓝牙外设 / 显示器 + 可移动存储
 *   find      指定设备查询：name / class / id 至少一个条件
 */

export default function registerDriver(pi: ExtensionAPI) {
	pi.registerTool({
		name: "driver",
		label: "Device Driver",
		description:
			"只读设备与驱动健康盘点，按 scope 选择子功能（不传默认 problem）：problem=异常设备（Status Error/Unknown，附硬件 ID 与原始错误码，不翻译）；core=Net/Bluetooth/Audio/显示四类硬件现状 + bthserv/Audiosrv 服务状态 + 驱动版本日期（供联网比对）；external=在线外设（USB/蓝牙外设/显示器）与可移动存储（U盘/移动硬盘/SD 卡）；find=按 name/class/id 定位具体设备。与 sys 的边界：sys 管负载，driver 管健康；只读，不改驱动不启停设备。" +
			" 输出 JSON 最多 50 KiB，超限标注 outputTruncated；请缩小查询范围获取省略内容。",
		promptSnippet:
			"Read-only device and driver health (WMI): problem devices with hardware IDs and raw error codes, network/Bluetooth/audio/display status with driver versions, connected USB/Bluetooth peripherals and removable storage, and device lookup by name/class/id",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Use driver scope=problem (default) when a device is missing, malfunctioning, or would show a yellow warning icon: lists PnP devices with Error/Unknown status plus hardware IDs and raw ConfigManagerErrorCode.",
			"Use scope=core for network/Bluetooth/audio/display health: adapters with a physical/virtual flag, bthserv and Audiosrv service state, and driver versions/dates to compare against the latest versions online.",
			"Use scope=external when a USB/Bluetooth peripheral or removable drive is not being recognized.",
			"Use scope=find with name, class, or id to locate one specific device (touchpad, fingerprint reader, built-in camera, a VID/PID).",
		],
		parameters: Type.Object({
			scope: Type.Optional(StringEnum(SCOPES)),
			name: Type.Optional(Type.String({ description: "scope=find 可选，设备名称子串（如 touchpad、webcam）。" })),
			class: Type.Optional(
				Type.String({ description: "scope=find 可选，设备类精确名（固定英文类名，如 Net、MEDIA、Bluetooth）。" }),
			),
			id: Type.Optional(
				Type.String({
					description:
						"scope=find 可选，硬件 ID / DeviceID 子串（如 VID_045E），对 deviceId 与 hardwareIds 双通道匹配。",
				}),
			),
		}),

		async execute(_toolCallId, params, signal, _onUpdate, _ctx) {
			signal?.throwIfAborted();
			const scope = params.scope ?? "problem";
			const payload = await runScope(params, signal);
			throwOnError(payload);
			const result = { [scope]: payload };
			signal?.throwIfAborted();
			return diagnosticResult(result);
		},
	});
}
