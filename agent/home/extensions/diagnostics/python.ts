import { StringEnum } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { collectPython, type PythonScope } from "./python-core.ts";
import { diagnosticResult, OUTPUT_GUIDELINE, throwOnError } from "./result.ts";

export default function registerPython(pi: ExtensionAPI) {
	pi.registerTool({
		name: "python",
		label: "Python Environments",
		description:
			"只读盘点 Windows 上的 Python 安装、虚拟环境、Conda 根目录与包管理入口。" +
			"scope=overview：PATH/启动器命令、环境变量、是否有多个 Python；" +
			"scope=installations：Python、Conda、Mamba、uv、Poetry、Pyenv 等入口及路径；" +
			"scope=environments：已知虚拟环境、Conda 环境及待核查清理清单；" +
			"scope=config：Conda、pipx、Poetry、Pyenv、uv 的配置输出；scope=all：一次返回全部。" +
			" 工具只读，不删除环境；清理必须由用户确认后用 runbook 生成逐项命令。",
		promptSnippet:
			"Audit Python installations, Conda/miniconda roots, virtualenvs, managers, PATH conflicts, and cleanup review candidates (read-only)",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Use python when the user reports multiple Python/Conda installations, wrong pip targets, conflicting virtual environments, PyMOL pulling a full Conda tree, or wants to clean up Python environments.",
			"Use scope=overview first for a quick diagnosis. Use scope=environments with includeSizes=true only when the user wants a cleanup inventory.",
			"Never delete an environment from this tool. Review active/project/known-purpose environments, then use runbook with explicit user confirmation.",
		],
		parameters: Type.Object({
			scope: StringEnum([
				"overview",
				"installations",
				"environments",
				"config",
				"all",
			] as const satisfies readonly PythonScope[]),
			path: Type.Optional(
				Type.String({
					description: "可选，额外扫描的项目目录。省略时扫描 pi 当前工作目录；深度由 maxDepth 控制。",
				}),
			),
			maxDepth: Type.Optional(
				Type.Number({
					description: "扫描项目目录下环境标记的最大深度，默认 3，范围 1～5。",
					minimum: 1,
					maximum: 5,
				}),
			),
			includeSizes: Type.Optional(
				Type.Boolean({
					description: "是否递归计算环境占用；默认 false。大环境可能增加数秒耗时。",
				}),
			),
		}),
		async execute(_toolCallId, params, signal, _onUpdate, ctx) {
			signal?.throwIfAborted();
			const result = await collectPython(
				{
					scope: params.scope as PythonScope,
					path: params.path,
					maxDepth: params.maxDepth ?? 3,
					includeSizes: params.includeSizes ?? false,
				},
				ctx.cwd,
				signal,
			);
			throwOnError(result as { error?: unknown });
			signal?.throwIfAborted();
			return diagnosticResult(result);
		},
	});
}
