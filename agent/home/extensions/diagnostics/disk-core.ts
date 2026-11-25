import type { Dirent } from "node:fs";
import { existsSync, promises as fsp, statfsSync } from "node:fs";
import { join, resolve } from "node:path";
import { psString } from "./pwsh-data.ts";
import { throwOnError } from "./result.ts";
import { asRecord, asRecords, createPwshRunner, errorMessage } from "./runtime.ts";

/**
 * disk - 只读磁盘信息工具（cst-pilot 定制）
 *
 * 结构只读：无任何写路径。
 * - scope=space : 纯 Node fs.statfsSync，零子进程
 * - scope=info  : 调本仓库自带的 pwsh\（命令串写死，模型输入仅作白名单校验后插值）
 * - scope=health: SMART 可靠性计数器（需管理员；失败时优雅降级并说明）
 * - scope=usage : 目录大小排行（Node 逐文件 stat 累加；50 万条目熔断，结果为下界）
 */

const DRIVE_LETTERS = "CDEFGHIJKLMNOPQRSTUVWXYZ".split("");

const runPwsh = createPwshRunner({ timeoutMs: 15000 });

const DISK_INFO_CMD =
	"$ErrorActionPreference='Stop'; ConvertTo-Json -InputObject @(Get-PhysicalDisk | Select-Object FriendlyName,SerialNumber,MediaType,BusType,HealthStatus,OperationalStatus,Size,DeviceId) -Depth 3";

const VOLUME_CMD =
	"$ErrorActionPreference='Stop'; ConvertTo-Json -InputObject @(Get-CimInstance Win32_LogicalDisk | Select-Object DeviceID,VolumeName,FileSystem,DriveType,Size,FreeSpace) -Depth 3";

// 盘符→物理盘关联（drive 过滤 info 用）：CIM 引用属性是对象，必须 ToString() 投影成字符串，
// 形态如 Win32_LogicalDisk (DeviceID = "C:") / Win32_DiskPartition (DeviceID = "Disk #1, Partition #1")
const DISK_ASSOC_CMD =
	"$ErrorActionPreference='Stop'; ConvertTo-Json -InputObject @(Get-CimInstance Win32_LogicalDiskToPartition | Select-Object @{n='Dep';e={$_.Dependent.ToString()}},@{n='Ant';e={$_.Antecedent.ToString()}}) -Depth 3";

function rows(value: unknown): Record<string, unknown>[] {
	if (value == null) return [];
	if (Array.isArray(value)) return asRecords(value);
	const row = asRecord(value);
	throwOnError(row);
	return [row];
}

async function physicalIds(drive: string, signal?: AbortSignal): Promise<string[]> {
	signal?.throwIfAborted();
	const assoc = rows(await runPwsh(DISK_ASSOC_CMD, { signal }));
	const ids = new Set<string>();
	for (const item of assoc) {
		signal?.throwIfAborted();
		const volume = String(item.Dep ?? "").match(/DeviceID\s*=\s*"([A-Za-z]:)"/);
		if (volume?.[1].toUpperCase() !== `${drive}:`) continue;
		const disk = String(item.Ant ?? "").match(/Disk #(\d+)/i);
		if (disk) ids.add(disk[1]);
	}
	if (!ids.size) throw new Error("无法确定目标卷对应的物理盘");
	return [...ids];
}

const buildSmartCommand = (ids?: string[]) => `
$ErrorActionPreference = 'Stop'
$data = @(); $failures = @(); $permissionDenied = $false
function Test-AccessDenied($e) {
  return ($e.CategoryInfo.Category -eq 'PermissionDenied' -or $e.Exception -is [UnauthorizedAccessException] -or $e.Exception.HResult -eq -2147024891)
}
try {
  $disks = @(Get-PhysicalDisk)
  ${ids ? `$disks = @($disks | Where-Object { @(${ids.map(psString).join(",")}) -contains [string]$_.DeviceId })` : ""}
  if (-not $disks.Count) { throw '未找到可查询的物理盘' }
  foreach ($disk in $disks) {
    try {
      $sample = @(Get-StorageReliabilityCounter -PhysicalDisk $disk -ErrorAction Stop | Select-Object DeviceId,Wear,Temperature,PowerOnHours,ReadErrorsTotal,WriteErrorsTotal)
      if (-not $sample.Count) { throw '设备未返回可靠性计数器' }
      $data += $sample
    } catch {
      $permissionDenied = $permissionDenied -or (Test-AccessDenied $_)
      $failures += @{ deviceId = [string]$disk.DeviceId; error = $_.Exception.Message }
    }
  }
} catch {
  $permissionDenied = $permissionDenied -or (Test-AccessDenied $_)
  $failures += @{ error = $_.Exception.Message }
}
ConvertTo-Json @{ data = $data; errors = $failures; permissionDenied = $permissionDenied } -Depth 4
`;

function toGiB(bytes?: unknown): number | null {
	return typeof bytes === "number" && Number.isFinite(bytes) && bytes >= 0 ? +(bytes / 2 ** 30).toFixed(1) : null;
}

/** usage 专用：GB 保留两位小数；小到两位归零的真实数据自动提升精度（绝不显示假 0） */
function toGiBWithAdaptivePrecision(n: number): number {
	const gb = n / 2 ** 30;
	if (gb <= 0) return 0;
	const r2 = +gb.toFixed(2);
	if (r2 > 0) return r2;
	const r4 = +gb.toFixed(4);
	if (r4 > 0) return r4;
	return +gb.toFixed(6);
}

function diskSpace(driveFilter: string) {
	const out: { drive: string; totalGB: number; freeGB: number; usedPct: number }[] = [];
	for (const L of DRIVE_LETTERS) {
		if (driveFilter && !L.startsWith(driveFilter[0])) continue;
		try {
			const s = statfsSync(`${L}:/`);
			out.push({
				drive: `${L}:`,
				totalGB: +((s.blocks * s.bsize) / 2 ** 30).toFixed(1),
				freeGB: +((s.bfree * s.bsize) / 2 ** 30).toFixed(1),
				usedPct: +(100 * (1 - s.bfree / s.blocks)).toFixed(1),
			});
		} catch {
			/* 盘符不存在，跳过 */
		}
	}
	return out;
}

/* ------------------------------------------------------------------ */
/* scope=usage · Node 逐文件 stat 累加                                 */
/* ------------------------------------------------------------------ */

const WALK_BUDGET = 500000; // stat 次数熔断

async function usageViaWalk(rootPath: string, topN: number, signal?: AbortSignal): Promise<Record<string, unknown>> {
	signal?.throwIfAborted();
	const denied: string[] = [];
	const dirSizes = new Map<string, number>();
	const truncatedDirs = new Set<string>();
	let statCount = 0;
	let truncated = false;

	async function walk(dir: string, signal?: AbortSignal): Promise<number> {
		signal?.throwIfAborted();
		let entries: Dirent[] | undefined;
		try {
			entries = await fsp.readdir(dir, { withFileTypes: true });
		} catch (e) {
			signal?.throwIfAborted();
			if (denied.length < 50) denied.push(`${dir} (${e instanceof Error && "code" in e ? e.code : "ERR"})`);
			truncatedDirs.add(dir);
			return 0;
		}
		let sum = 0;
		for (const ent of entries) {
			signal?.throwIfAborted();
			if (statCount >= WALK_BUDGET) {
				truncated = true;
				truncatedDirs.add(dir);
				break;
			}
			if (ent.isSymbolicLink()) continue; // 防 junction/符号链接环路
			const full = join(dir, ent.name);
			if (ent.isDirectory()) {
				sum += await walk(full, signal);
				statCount++;
			} else if (ent.isFile()) {
				try {
					const st = await fsp.stat(full);
					sum += st.size;
				} catch {
					signal?.throwIfAborted();
					/* 单文件不可读，忽略 */
				}
				statCount++;
			}
		}
		dirSizes.set(dir, sum);
		return sum;
	}

	const t0 = Date.now();
	const totalBytes = await walk(rootPath, signal);
	const elapsed = ((Date.now() - t0) / 1000).toFixed(1);

	const topDirs = [...dirSizes.entries()]
		.filter(([p]) => p.replace(/[\\/]+$/, "").toUpperCase() !== rootPath.replace(/[\\/]+$/, "").toUpperCase())
		.sort((a, b) => b[1] - a[1])
		.slice(0, topN)
		.map(([p, bytes]) => ({
			path: p,
			sizeGB: toGiBWithAdaptivePrecision(bytes),
			pct: totalBytes > 0 ? +((100 * bytes) / totalBytes).toFixed(1) : null,
		}));

	return {
		method: "node-walk",
		root: rootPath,
		totalGB: toGiBWithAdaptivePrecision(totalBytes),
		topDirs,
		stats: { filesScanned: statCount, elapsedSec: +elapsed, budget: WALK_BUDGET },
		truncated,
		denied: denied.length ? denied : undefined,
		notice: truncated
			? "达到扫描预算上限，提前停止：所有数值为部分统计（真实值 ≥ 显示值，下界）。可用更深的子路径分别查询。"
			: "逐文件统计完成，数值为遍历所得（系统拒绝访问的目录未计入）。",
	};
}

/* ------------------------------------------------------------------ */

export interface DiskParams {
	scope: "space" | "info" | "health" | "usage" | "all";
	drive?: string;
	path?: string;
	top?: number;
}
export async function collectDisk(
	params: DiskParams,
	cwd: string,
	signal?: AbortSignal,
): Promise<Record<string, unknown>> {
	signal?.throwIfAborted();
	// 参数白名单二次校验：模型只能传盘符，其余一切拒绝
	const drive = (params.drive ?? "").toUpperCase().replace(/[^A-Z]/g, "");
	if (params.drive && !/^[A-Z]$/.test(drive)) {
		throw new Error(`非法盘符参数: ${JSON.stringify(params.drive)}（只接受单个字母 A-Z）`);
	}

	const result: Record<string, unknown> = {};
	const scope = params.scope;

	if (scope === "space" || scope === "all") {
		result.space = diskSpace(drive);
	}

	let mappedIds: string[] | undefined;
	let mappingError: string | undefined;
	if (drive && ["info", "health", "all"].includes(scope)) {
		try {
			mappedIds = await physicalIds(drive, signal);
		} catch (e) {
			signal?.throwIfAborted();
			mappingError = errorMessage(e);
		}
	}
	if (scope === "info" || scope === "all") {
		const phys = await runPwsh(DISK_INFO_CMD, { signal });
		const vols = await runPwsh(VOLUME_CMD, { signal });
		if (!Array.isArray(phys) && asRecord(phys).error) result.physicalDisks = phys;
		else {
			const selected = rows(phys).filter((d) => !mappedIds || mappedIds.includes(String(d.DeviceId)));
			result.physicalDisks = selected.map((d) => ({ ...d, sizeGB: toGiB(d.Size), Size: undefined }));
			if (mappingError)
				result.infoNotice = `盘符关联失败（${mappingError}），physicalDisks 为未过滤全量清单；volumes 仍按盘符过滤`;
		}
		result.volumes =
			!Array.isArray(vols) && asRecord(vols).error
				? vols
				: rows(vols)
						.filter((v) => !drive || String(v.DeviceID).toUpperCase() === `${drive}:`)
						.map((v) => ({
							drive: v.DeviceID,
							label: v.VolumeName,
							fs: v.FileSystem,
							driveType:
								({ 2: "Removable", 3: "Fixed", 4: "Network", 5: "Optical" } as Record<number, string>)[
									Number(v.DriveType)
								] ?? v.DriveType,
							totalGB: toGiB(v.Size),
							freeGB: toGiB(v.FreeSpace),
						}));
	}

	if (scope === "health" || scope === "all") {
		if (mappingError) {
			result.smart = null;
			result.smartNotice = `未查询 SMART：${mappingError}`;
		} else {
			const smart = asRecord(await runPwsh(buildSmartCommand(mappedIds), { timeoutMs: 20000, signal }));
			result.smart = smart?.error ? null : Array.isArray(smart.data) && smart.data.length ? smart.data : null;
			if (smart?.error || (Array.isArray(smart.errors) && smart.errors.length)) {
				result.smartErrors = smart.error ? [{ error: smart.error }] : smart.errors;
				result.smartNotice =
					"SMART 部分或全部采集失败，原因见 smartErrors。" +
					(smart.permissionDenied ? "检测到访问拒绝，可尝试以管理员身份重试。" : "");
			}
		}
	}

	if (scope === "usage") {
		const inputPath = (params.path ?? "").trim();
		const rootPath = inputPath ? resolve(cwd, inputPath) : "";
		if (!rootPath) {
			result.usage = { error: 'scope=usage 需要提供 path，如 "C:\\" 或 "D:\\Data"' };
		} else if (!existsSync(rootPath)) {
			result.usage = { error: `路径不存在: ${rootPath}` };
		} else {
			const topN = Math.max(1, Math.min(100, Math.floor(params.top ?? 20)));
			result.usage = await usageViaWalk(rootPath, topN, signal);
		}
	}

	signal?.throwIfAborted();
	if (scope === "usage") throwOnError(asRecord(result.usage));
	if (scope === "info" && !Array.isArray(result.physicalDisks) && !Array.isArray(result.volumes)) {
		throw new Error(`磁盘信息采集失败：${asRecord(result.physicalDisks).error}；${asRecord(result.volumes).error}`);
	}
	if (scope === "health" && result.smart === null && (mappingError || result.smartErrors)) {
		throw new Error(`${result.smartNotice} ${JSON.stringify(result.smartErrors ?? [])}`);
	}
	if (
		mappingError ||
		result.smartErrors ||
		(result.physicalDisks && !Array.isArray(result.physicalDisks)) ||
		(result.volumes && !Array.isArray(result.volumes))
	) {
		result.degraded = true;
	}
	return result;
}
