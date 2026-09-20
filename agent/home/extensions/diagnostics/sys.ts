import { existsSync } from "node:fs";
import { join } from "node:path";
import { StringEnum } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { collectionNotice } from "./pwsh-data.ts";
import { diagnosticResult, OUTPUT_GUIDELINE, throwOnError } from "./result.ts";
import { asRecord, createPwshRunner, execFileP } from "./runtime.ts";
import {
	buildGpuCommand,
	buildIoCommand,
	buildProcessCommand,
	LHM_DLL,
	LHM_GPU_CMD,
	OVERVIEW_CMD,
	SENSOR_CMD,
} from "./sys-commands.ts";

/**
 * sys - 只读系统检查工具（cst-pilot 定制）
 *
 * 结构只读：无任何写路径，注册表 / 系统配置零改动。
 * - scope=proc : 进程盘点。内存取 Get-Process 快照；CPU% 用 1.2s 双采样差分
 *   （不走 PerfProc 原始计数器表，实测该路径 7.8s 过慢）
 * - scope=gpu  : 每进程 GPU 利用率（GPU Engine 计数器按 pid 聚合，
 *   偶发无效采样时自动重试 1 次）+ 每进程专用显存（GPU Process Memory）
 *   + 显卡适配器清单（Win32_VideoController，含虚拟显示）；检测到 nvidia-smi
 *   （系统驱动自带）则附显卡温度 / 功耗 / 显存 / 驱动版本；无 NVIDIA 时改用
 *   LHM 用户态附核显 / 其他卡传感器（lhmGpu）。均不需要管理员权限。
 * - scope=io   : 每物理盘队列 / 忙碌 / 吞吐 + 每进程读写 IO 速率 Top N
 *   （Win32_Process 累计 IO 计数差分，避开 PerfProc 慢路径）。免管理员。
 * - scope=sensor: 温度 / 风扇 / 电压，数据源 LibreHardwareMonitorLib（LHM，
 *   开源硬件传感器库，DLL 打包在仓库 lhm\）。GPU 传感器免管理员；
 *   CPU / 主板传感器的内核级读取依赖 PawnIO 驱动（LHM 0.9.6 起不再内置
 *   WinRing0），未装 PawnIO 或非管理员时自动降级只报可用部分。
 * - scope=overview: 整机负载概况（R4）。物理内存用量 + CPU 总占用率 +
 *   页面文件状态 + 开机时长 + 内核内存池 + 机型信息
 *   （厂商 / 型号 / CPU / BIOS）。免管理员，纯快照。
 *
 * 里程碑（doc/design/tool/sys-design.md）：R1=proc，R2=gpu，R3=sensor，R4=overview。
 * R5（开机自启盘点）已从 sys 剥离为独立工具 startup.ts（配置盘点与实时
 * 负载不属一类问题，单独注册边界更清晰）。无 scope 时兜底 overview。
 */

const NVIDIA_SMI = join(process.env.WINDIR ?? "C:\\Windows", "System32", "nvidia-smi.exe");

const runPwsh = createPwshRunner({ timeoutMs: 20000, diagnostics: true });

/* ------------------------------------------------------------------ */
/* scope=proc · 进程盘点：快照 + 1.2s 双采样差分                        */
/* ------------------------------------------------------------------ */

async function collectProc(topN: number, signal?: AbortSignal) {
	signal?.throwIfAborted();
	const r = asRecord(await runPwsh(buildProcessCommand(topN), { signal }));
	if (r && typeof r.error === "string") return { error: r.error };
	return {
		...r,
		notice: `${collectionNotice(r)}进程 ${r.totalProcs} 个，采样间隔 ${r.intervalSec}s（${r.cores} 逻辑核）。byCpu=CPU 占用率 Top N；byMem=内存（工作集）Top N。cpuPct 为采样窗口内的平均值，瞬时突发可能低估。path=可执行文件路径（系统进程或权限不足时为 null，可用于就地验证进程身份）。`,
	};
}

/* ------------------------------------------------------------------ */
/* scope=gpu · 每进程 GPU 利用率 + 专用显存 + nvidia-smi 附带            */
/* ------------------------------------------------------------------ */

/** nvidia-smi 系统驱动自带：存在则附带显卡状态，不存在返回 null */
async function nvidiaStatus(signal?: AbortSignal) {
	signal?.throwIfAborted();
	if (!existsSync(NVIDIA_SMI)) return null;
	try {
		const r = await execFileP(
			NVIDIA_SMI,
			[
				"--query-gpu=uuid,pci.bus_id,name,temperature.gpu,power.draw,memory.used,memory.total,utilization.gpu,driver_version",
				"--format=csv,noheader,nounits",
			],
			{ signal, timeout: 8000, windowsHide: true, encoding: "utf8" },
		);
		const number = (value: string): number | null => {
			const n = Number(value);
			return value && Number.isFinite(n) ? n : null;
		};
		const rows = r.stdout
			.trim()
			.split(/\r?\n/)
			.filter(Boolean)
			.map((line) => {
				const [uuid, pciBusId, name, temp, power, memUsed, memTotal, util, driver] = line
					.split(",")
					.map((x) => x.trim());
				if (!driver) throw new Error("nvidia-smi 返回格式异常");
				return {
					uuid,
					pciBusId,
					name,
					tempC: number(temp),
					powerW: number(power),
					vramUsedMB: number(memUsed),
					vramTotalMB: number(memTotal),
					utilPct: number(util),
					driver,
				};
			});
		if (!rows.length) return { error: "nvidia-smi 未返回显卡" };
		return rows;
	} catch {
		signal?.throwIfAborted();
		return { error: "nvidia-smi 调用失败" };
	}
}

/** LHM 只开 GPU 的精简查询：无 NVIDIA 独显时的核显 / 其他卡降级路径。
 *  部分老核显 LHM 读不出传感器——返回空 hardware/sensors，
 *  语义区别于 nvidia: null（"未检出 NVIDIA 独显"），由 notice 说明。 */

async function lhmGpuStatus(signal?: AbortSignal) {
	signal?.throwIfAborted();
	if (!existsSync(LHM_DLL)) return { hardware: [], sensors: [], notice: "LHM DLL 缺失，GPU 传感器不可读" };
	const r = asRecord(await runPwsh(LHM_GPU_CMD, { timeoutMs: 20000, signal }));
	if (r && typeof r.error === "string")
		return { hardware: [], sensors: [], notice: `${collectionNotice(r)}LHM 读取失败: ${r.error}` };
	return { hardware: Array.isArray(r.hardware) ? r.hardware : [], sensors: Array.isArray(r.sensors) ? r.sensors : [] };
}

async function collectGpu(topN: number, signal?: AbortSignal) {
	signal?.throwIfAborted();
	// GPU Engine 计数器偶发无效采样（实测观察到过）：失败重试 1 次再收敛为 {error}。
	// 重试会重跑 buildGpuCommand（适配器清单随重跑更新）；nvidia-smi 只在首次调用，结果复用。
	const [rawFirst, nv] = await Promise.all([runPwsh(buildGpuCommand(topN), { signal }), nvidiaStatus(signal)]);
	const first = asRecord(rawFirst);
	const r =
		first.error || asRecord(first.counterErrors ?? {}).engine
			? asRecord(await runPwsh(buildGpuCommand(topN), { signal }))
			: first;
	const out: Record<string, unknown> = { ...r, nvidia: nv };
	if (!nv || !Array.isArray(nv)) out.lhmGpu = await lhmGpuStatus(signal);
	if (r.error && !Array.isArray(nv)) {
		const lhm = asRecord(out.lhmGpu ?? {});
		if (!Array.isArray(lhm.sensors) || lhm.sensors.length === 0) throwOnError(r);
	}
	if (
		r?.error ||
		asRecord(r.counterErrors ?? {}).engine ||
		asRecord(r.counterErrors ?? {}).memory ||
		(nv && !Array.isArray(nv) ? nv.error : undefined)
	)
		out.degraded = true;
	out.notice =
		collectionNotice(out) +
		`GPU Engine ${r.engineSamples ?? 0} 个实例按进程聚合。byGpuPct=GPU 利用率 Top N（engtypes=所用引擎类型，如 3d/copy/videodecode）；byDedicatedMB=专用显存 Top N；adapters=显卡适配器清单（bus=PCI 为实体卡插槽设备，ROOT 多为虚拟显示适配器；真实显卡以 vendor 为硬件厂商的那条为准）；` +
		(Array.isArray(nv)
			? `nvidia=NVIDIA 显卡状态数组，按 uuid/pciBusId 区分每张卡。`
			: nv?.error
				? `nvidia.error=NVIDIA 状态采集失败，传感器补充见 lhmGpu。`
				: `nvidia=null=nvidia-smi 不存在；GPU 传感器见 lhmGpu，hardware 为空不代表没有显卡。`) +
		`gpuPct 为该进程跨适配器最繁忙引擎的利用率，不累加并行引擎；engines 保留各适配器/引擎的样本。计数器或数据源失败见 counterErrors/error，其他结果仍可用。`;
	return out;
}

/* ------------------------------------------------------------------ */
/* scope=io · 磁盘 IO 定位：每盘队列 / 吞吐 + 每进程 IO 速率             */
/* ------------------------------------------------------------------ */

// "电脑卡但 CPU 内存都闲"的最常见原因是磁盘 IO 打满。两路数据共用同一个
// 1.2s 采样窗口，一条 pwsh 命令取全：
// 1) 每进程 IO：Win32_Process 的 Read/WriteTransferCount 是进程启动以来
//    的累计值，两次快照差分即速率。不走 PerfProc 计数器表（实测 7.8s 过慢，
//    同 proc scope 的教训）；Win32_Process 是普通 CIM 枚举，快。
// 2) 每盘队列：Win32_PerfFormattedData_PerfDisk_PhysicalDisk 格式化计数器类
//    （类名不随系统语言本地化，同 overview 模式），首读丢弃取第二次。

async function collectIo(topN: number, signal?: AbortSignal) {
	signal?.throwIfAborted();
	const r = asRecord(await runPwsh(buildIoCommand(topN), { timeoutMs: 20000, signal }));
	if (r && typeof r.error === "string") return { error: r.error };
	return {
		...r,
		notice: `${collectionNotice(r)}disks=每物理盘实时 IO（busyPct=磁盘忙碌百分比，持续 >80 或 queueLen 持续 >1 = IO 瓶颈；readKBs/writeKBs=读写吞吐）；byIo=每进程读+写 IO 速率 Top N（${r.intervalSec}s 差分，仅列有活动的进程，瞬时空闲可能无条目）。与 disk 的分工：disk 管容量与硬件健康，io 管"现在谁在读写"。`,
	};
}

/* ------------------------------------------------------------------ */
/* scope=sensor · 温度 / 风扇 / 电压 / 降频（LHM 用户态 + 系统计数器）     */
/* ------------------------------------------------------------------ */

// 三路数据，全部免安装：
// 1) LHM 0.9.6 用户态部分（GPU 走 NVAPI）：DLL 随仓库分发，Add-Type 加载。
//    CPU/主板传感器需要 PawnIO 内核驱动（LHM 0.9.5 起不再内置 WinRing0），
//    零宿主安装约束下不提供——用降频信号（路 3）替代过热诊断。
// 2) 热区：Thermal Zone Information 性能计数器，免管理员（MSAcpi WMI 反而要
//    管理员，且数据同源，弃用）。温度开尔文，% Passive Limit < 100 表示
//    该热区正在被动降热。
// 3) 降频：% of Maximum Frequency 各核当前频率占最大频率百分比。
//    低值 + 高负载 = 过热/功耗降频的直接信号。

async function collectSensor(signal?: AbortSignal) {
	signal?.throwIfAborted();
	if (!existsSync(LHM_DLL)) {
		return { error: `未找到 LHM DLL（${LHM_DLL}），sensor scope 不可用` };
	}
	const r = asRecord(await runPwsh(SENSOR_CMD, { timeoutMs: 30000, signal }));
	if (r && typeof r.error === "string") return { error: r.error };
	const hwList = Array.isArray(r.hardware) ? r.hardware.join(" / ") : "";
	const pawnioFull = r.pawnio && r.admin ? "已检测到 PawnIO + 管理员，CPU/主板传感器已包含在 sensors 中。" : "";
	// 计数器失败不静默：透出原因。无法区分"机器本来就没有"与"偶发失败"，
	// 重试无判据，故不自动重试，让模型知情后自行决定。
	const cErr = asRecord(r.counterErrors ?? {});
	const out: Record<string, unknown> = {
		degraded: r.degraded,
		collectionErrors: r.collectionErrors,
		admin: r.admin,
		pawnio: r.pawnio,
		hardware: r.hardware,
		sensorCount: Array.isArray(r.sensors) ? r.sensors.length : 0,
		sensors: r.sensors,
		thermalZones: r.thermalZones,
		frequency: r.frequency,
	};
	if (cErr.thermal || cErr.frequency) out.counterErrors = cErr;
	out.notice =
		collectionNotice(r) +
		`sensors=LHM 可读传感器（GPU 等，免管理员）；thermalZones=主板热区（passivePct<100 表示该热区正在被动降热）；` +
		`frequency=各核频率占最大频率百分比（minPctOfMax 低 + 负载高 = 过热/功耗降频的直接信号）。` +
		`CPU 核心温度需内核驱动（PawnIO），零安装约束下不可得，用降频信号替代。${pawnioFull}` +
		(cErr.thermal || cErr.frequency
			? `注意：计数器部分读取失败（${[cErr.thermal, cErr.frequency].filter(Boolean).join("；")}），对应字段为空是读取失败，不代表机器没有热区/降频计数器。`
			: "") +
		(hwList ? `（硬件：${hwList}）` : "");
	return out;
}

/* ------------------------------------------------------------------ */
/* scope=overview · 整机负载概况：内存 / CPU 总占用 / 页面文件 / 开机时长 */
/* ------------------------------------------------------------------ */

// 免管理员，纯快照（近 1 秒差分除外，见下）。
// CPU 总占用：Win32_PerfFormattedData_PerfOS_Processor（WMI 格式化计数器类，
// 类名不随系统语言本地化，避开 Get-Counter 英文路径在本机可用的脆弱依赖）。
// 该类首读是 provider 启动以来的累计值不可信，读两次取第二次。

async function collectOverview(signal?: AbortSignal) {
	signal?.throwIfAborted();
	const r = asRecord(await runPwsh(OVERVIEW_CMD, { timeoutMs: 20000, signal }));
	if (r && typeof r.error === "string") return { error: r.error };
	return {
		...r,
		notice: `${collectionNotice(r)}整机负载快照。mem=物理内存用量（usedPct>90 提示内存吃紧，可与 proc.byMem 对照找大户）；cpuTotalPct=整机 CPU 占用率（近 1 秒差分，可与 proc.byCpu 对照）；pagefile=页面文件分配/当前/峰值用量（usedMB 持续接近 allocMB 说明物理内存不足在靠页面文件撑）；pool=内核内存池（nonpaged 不可换出，持续异常增长多为驱动泄漏——内存高但进程榜单对不上大户时看这里）；machine=机型（vendor/model/cpu/bios，现场按机型匹配已知问题：散热缺陷、OEM 预装坑）；uptime=开机时长。`,
	};
}

/* ------------------------------------------------------------------ */

export default function registerSys(pi: ExtensionAPI) {
	pi.registerTool({
		name: "sys",
		label: "System Check",
		description:
			"只读系统检查工具，按 scope 选择子功能（不传默认 overview）：overview=整机负载快照（内存/CPU/页面文件/内存池/机型/开机时长）；proc=进程内存与 CPU 占用 Top N（含可执行文件路径）；io=每盘队列/吞吐 + 每进程 IO 速率 Top N（磁盘瓶颈定位）；gpu=每进程 GPU 利用率与显存排行（附适配器清单；有 NVIDIA 时附显卡状态，无 NVIDIA 时附核显/其他卡传感器）；sensor=温度/风扇/电压/降频信号（过热诊断）。详细指南与诊断交叉印证链见 skill「sys」。" +
			" 输出 JSON 最多 50 KiB，超限标注 outputTruncated；请缩小查询范围获取省略内容。",
		promptSnippet:
			"Query overall system load, running processes, disk IO, GPU load, and hardware sensors (read-only)",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Use sys scope=overview (or omit scope) when the user asks whether the machine is loaded/sluggish overall: gives RAM usage, total CPU load, pagefile pressure, kernel memory pool, machine model (vendor/model/CPU/BIOS), and uptime in one snapshot.",
			"Use sys scope=proc when the user asks who is using memory/CPU or whether a process is hogging resources.",
			"Use sys scope=io when the machine feels slow but CPU and memory look idle: shows per-disk busy/queue/read-write throughput and which processes are doing disk IO.",
			"Use sys scope=gpu when the user asks about GPU load, VRAM usage, GPU temperature, or which graphics card the machine has (adapters list shows real and virtual display adapters).",
			"Use sys scope=sensor when the user asks about temperatures, fans, voltages, or overheating (provides GPU sensors, thermal zones, and CPU throttling percentage; CPU core temps need a kernel driver and are unavailable).",
		],
		parameters: Type.Object({
			scope: Type.Optional(StringEnum(["overview", "proc", "gpu", "sensor", "io"] as const)),
			top: Type.Optional(Type.Number({ description: "可选，Top N 进程数，默认 10，上限 50。" })),
		}),

		async execute(_toolCallId, params, signal, _onUpdate, _ctx) {
			signal?.throwIfAborted();
			const topN = Math.max(1, Math.min(50, Math.floor(params.top ?? 10)));
			const scope = params.scope ?? "overview"; // 无 scope 兜底 overview：一次调用回答“电脑现在怎么样”
			const result: Record<string, unknown> = {};

			if (scope === "proc") {
				result.proc = await collectProc(topN, signal);
			} else if (scope === "gpu") {
				result.gpu = await collectGpu(topN, signal);
			} else if (scope === "sensor") {
				result.sensor = await collectSensor(signal);
			} else if (scope === "overview") {
				result.overview = await collectOverview(signal);
			} else if (scope === "io") {
				result.io = await collectIo(topN, signal);
			} else {
				result.error = `未知 scope: ${scope}（当前支持 overview / proc / gpu / sensor / io）`;
			}

			signal?.throwIfAborted();
			const payload = asRecord(result[scope]);
			if (scope !== "gpu") throwOnError(payload);
			return diagnosticResult(result);
		},
	});
}
