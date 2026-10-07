import { randomUUID } from "node:crypto";
import type { Dirent } from "node:fs";
import { createReadStream, existsSync, promises as fsp, mkdirSync, rmSync, statfsSync } from "node:fs";
import { join, resolve } from "node:path";
import * as readline from "node:readline";
import { psString } from "./pwsh-data.ts";
import { throwOnError } from "./result.ts";
import {
	asRecord,
	asRecords,
	createPwshRunner,
	decodeBuffer,
	errorMessage,
	execFileP,
	WIZTREE,
	WIZTREE_TMP,
} from "./runtime.ts";
import { FILE_INDEX_MIN_BYTES, getDriveKey, normalizePath, type VolumeIndex } from "./wz-index.ts";

/**
 * disk - 只读磁盘信息工具（cst-pilot 定制）
 *
 * 结构只读：无任何写路径（WizTree 导出的临时 CSV 写在自家 wiztree\tmp 并即时删除）。
 * - scope=space : 纯 Node fs.statfsSync，零子进程
 * - scope=info  : 调本仓库自带的 pwsh\（命令串写死，模型输入仅作白名单校验后插值）
 * - scope=health: SMART 可靠性计数器（需管理员；失败时优雅降级并说明）
 * - scope=usage : 目录大小排行
 *     · 管理员 + WizTree 便携版可用 -> 直读 NTFS MFT（秒级全盘）
 *     · 否则 -> Node 逐文件 stat 累加（较慢；50 万条目熔断，结果为下界）
 */

// 跨扩展共享的 WizTree 账本：usage 扫描顺手喂账，ls 直接吃现成

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
/* scope=usage · 快速路径：WizTree 直读 MFT（实测普通权限也可用）        */
/* ------------------------------------------------------------------ */

/** 僵尸文件阈值：≥50MB 且 ≥1 年未修改 */
const STALE_MIN_BYTES = 50 * 1024 * 1024;
const STALE_MIN_AGE_DAYS = 365;

/** 恒定内存的 Top-N 账本：只有超过当前末位门槛才插入并重排，
 *  其余数据流过即丢 —— 116 万行流式解析时内存不随数据量增长 */
function createTopN<T>(n: number, key: (x: T) => number) {
	const arr: T[] = [];
	return {
		add(item: T): void {
			const k = key(item);
			if (arr.length >= n) {
				if (k <= key(arr[arr.length - 1])) return; // 快速拒绝：低于末位门槛
				arr[arr.length - 1] = item;
			} else {
				arr.push(item);
			}
			arr.sort((a, b) => key(b) - key(a));
		},
		get(): T[] {
			return arr;
		},
	};
}

/** WizTree 导出的本地时间 "YYYY/MM/DD HH:MM:SS" → Date（解析失败返回 null） */
function parseWizDate(s: string): Date | null {
	const m = s.match(/(\d{4})\/(\d{2})\/(\d{2})\s+(\d{2}):(\d{2}):(\d{2})/);
	if (!m) return null;
	return new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]);
}

/* 卷文件系统判定：WizTree 的 MFT 捷径仅 NTFS 有效，FAT32/exFAT/UNC 上它实际走目录遍历，
 * method 必须按卷类型标注，否则 FAT32 上会冒出假的 "wiztree-mft"。结果按盘符缓存。
 * fsutil 快路径对 NTFS 系统卷可能拒绝访问（实测错误 5），失败时用 pwsh CIM 兜底。 */
const volFsCache = new Map<string, { identity: string; fs: string }>();
async function volumeFs(drive: string | null, index: VolumeIndex, signal?: AbortSignal): Promise<string | null> {
	signal?.throwIfAborted();
	if (!drive) return null; // UNC 无盘符，保守按非 NTFS 处理
	const key = drive.toUpperCase();
	const identity = await index.refreshVolume(drive);
	const hit = volFsCache.get(key);
	if (identity && hit?.identity === identity) return hit.fs;
	volFsCache.delete(key);
	let fs: string | null = null;
	try {
		const r = await execFileP("fsutil", ["fsinfo", "volumeinfo", drive], {
			signal,
			timeout: 15000,
			windowsHide: true,
			encoding: "buffer",
			maxBuffer: 1024 * 1024,
		});
		// FS 名称本身是英文，不随系统语言本地化，直接在输出里找
		const m = String(decodeBuffer(r.stdout as Buffer)).match(/(NTFS|FAT32|exFAT|FAT16|ReFS)/i);
		fs = m ? m[1].toUpperCase() : null;
	} catch {
		signal?.throwIfAborted();
		fs = null;
	}
	if (!fs) {
		// 兜底：CIM 查询普通权限可用（info scope 同源）
		// runPwsh 是 JSON 通道：裸字符串会解析失败被当错误，必须 ConvertTo-Json
		try {
			const r = await runPwsh(
				`(Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='${drive}'").FileSystem | ConvertTo-Json`,
				{ timeoutMs: 60000, signal },
			); // U 盘 pwsh 冷 spawn 慢，15s 会被掐
			const s = typeof r === "string" ? r : String(asRecord(r).stdout ?? "");
			const m = s.match(/(NTFS|FAT32|exFAT|FAT16|ReFS)/i);
			fs = m ? m[1].toUpperCase() : null;
		} catch {
			signal?.throwIfAborted();
			fs = null;
		}
	}
	if (identity && fs && (await index.refreshVolume(drive)) === identity) volFsCache.set(key, { identity, fs });
	return fs;
}

async function usageViaWizTree(
	rootPath: string,
	topN: number,
	index: VolumeIndex,
	signal?: AbortSignal,
): Promise<Record<string, unknown>> {
	signal?.throwIfAborted();
	const csvPath = join(WIZTREE_TMP, `export-${process.pid}-${randomUUID()}.csv`);
	try {
		mkdirSync(WIZTREE_TMP, { recursive: true });
		const scanDrive = getDriveKey(rootPath);
		const identity = scanDrive ? await index.refreshVolume(scanDrive) : null;
		// /exportfiles=1：文件行也导出 —— 大文件/扩展名/僵尸三本账的原料
		await execFileP(WIZTREE, [rootPath, "/admin=0", `/export=${csvPath}`, "/exportfolders=1", "/exportfiles=1"], {
			signal,
			timeout: 180000,
			windowsHide: true,
			maxBuffer: 1024 * 1024,
		});
		if (!existsSync(csvPath)) {
			return { error: "WizTree 未生成导出文件" };
		}

		// 流式逐行解析（不依赖表头，WizTree 表头随系统语言变化）：
		// 每行 "路径",字节数,... 进聚合器后即丢，内存恒定
		const norm = normalizePath;
		const rootKey = norm(rootPath);
		const cachedDirs = new Map<string, number>();
		const cachedFiles = new Map<string, number>();
		const drive = identity ? scanDrive : null; // 只有身份可确认的卷才入账
		const dirTop = createTopN<{ path: string; bytes: number }>(topN, (r) => r.bytes);
		const fileTop = createTopN<{ path: string; bytes: number }>(topN, (r) => r.bytes);
		const staleTop = createTopN<{ path: string; bytes: number; modified: string }>(topN, (r) => r.bytes);
		const extMap = new Map<string, { bytes: number; files: number }>();
		let totalBytes = 0;
		let rows = 0;
		let files = 0;
		const now = Date.now();

		// 行格式实测：目录行 "C:\",195695...,...,2026/08/30 23:32:41,0,...（日期裸露无引号）
		// 因此：宽松行头拿路径+字节；日期从行头之后的剩余串里找（带不带引号都能吃）
		const lineRe = /^"(.+)",(\d+),/;
		const dateRe = /(\d{4}\/\d{2}\/\d{2}\s+\d{2}:\d{2}:\d{2})/;
		const rl = readline.createInterface({
			input: createReadStream(csvPath, { signal, encoding: "utf-8" }),
			crlfDelay: Infinity,
		});
		for await (const line of rl) {
			signal?.throwIfAborted();
			const m = line.match(lineRe);
			if (!m) continue;
			const path = m[1];
			const bytes = +m[2];
			rows++;
			if (/[\\/]$/.test(path)) {
				// 目录行
				if (norm(path) === rootKey) totalBytes = bytes;
				else dirTop.add({ path, bytes });
				// 顺手喂共享账本（ls 直接吃现成，不再重复扫描）
				if (drive) cachedDirs.set(normalizePath(path), bytes);
				continue;
			}
			// 文件行：三本账
			files++;
			fileTop.add({ path, bytes });
			if (drive && bytes >= FILE_INDEX_MIN_BYTES) cachedFiles.set(normalizePath(path), bytes); // ≥1MB 才入账（wz-index 内筛选）
			const dot = path.lastIndexOf(".");
			const ext =
				dot > path.lastIndexOf("\\") && dot < path.length - 1
					? path
							.slice(dot + 1)
							.toLowerCase()
							.slice(0, 12)
					: "(无扩展名)";
			const agg = extMap.get(ext) ?? { bytes: 0, files: 0 };
			agg.bytes += bytes;
			agg.files++;
			extMap.set(ext, agg);
			const dm = line.slice(m[0].length).match(dateRe);
			const d = dm ? parseWizDate(dm[1]) : null;
			if (dm && d && bytes >= STALE_MIN_BYTES && now - d.getTime() >= STALE_MIN_AGE_DAYS * 86400000) {
				staleTop.add({ path, bytes, modified: dm[1] });
			}
		}
		if (rows === 0) return { error: "WizTree CSV 无数据行" };
		if (scanDrive && identity && (await index.refreshVolume(scanDrive)) !== identity)
			return { error: "扫描期间卷身份变化，请重试" };

		const fsName = await volumeFs(drive, index, signal);
		if (drive && identity && !(await index.commitIndex(drive, identity, rootPath, cachedDirs, cachedFiles, signal)))
			return { error: "扫描期间卷身份变化，请重试" };
		const isNtfs = fsName === "NTFS";
		// 判不出 FS（探测全失败）时不瞎标，按 wiztree-walk 保守标注并提示待确认
		const fsLabel = fsName ?? "未知（探测失败）";

		const extAgg = [...extMap.entries()]
			.map(([ext, v]) => ({ ext, files: v.files, sizeGB: toGiBWithAdaptivePrecision(v.bytes) }))
			.sort((a, b) => b.sizeGB - a.sizeGB)
			.slice(0, Math.min(topN, 40));

		return {
			method: isNtfs ? "wiztree-mft" : "wiztree-walk",
			root: rootPath,
			totalGB: totalBytes ? toGiBWithAdaptivePrecision(totalBytes) : null,
			topDirs: dirTop.get().map((r) => ({
				path: r.path,
				sizeGB: toGiBWithAdaptivePrecision(r.bytes),
				pct: totalBytes ? +((100 * r.bytes) / totalBytes).toFixed(1) : null,
			})),
			topFiles: fileTop.get().map((r) => ({
				path: r.path,
				sizeGB: toGiBWithAdaptivePrecision(r.bytes),
				pct: totalBytes ? +((100 * r.bytes) / totalBytes).toFixed(1) : null,
			})),
			extAgg,
			staleFiles: staleTop.get().map((r) => ({
				path: r.path,
				sizeGB: toGiBWithAdaptivePrecision(r.bytes),
				modified: r.modified,
			})),
			notice: isNtfs
				? `WizTree 全量 MFT 导出（${rows} 行，其中文件 ${files} 个）。topDirs=目录排行；topFiles=单个大文件；extAgg=按扩展名聚合（含文件数）；staleFiles=≥50MB 且 ≥1 年未修改的文件（大者优先）。全部只读统计。`
				: fsName
					? `WizTree 扫描：${fsLabel} 卷无 MFT，实际走目录遍历（${rows} 行，其中文件 ${files} 个），非 MFT 精确账。topDirs=目录排行；topFiles=单个大文件；extAgg=按扩展名聚合（含文件数）；staleFiles=≥50MB 且 ≥1 年未修改的文件（大者优先）。全部只读统计。`
					: `WizTree 扫描：卷文件系统探测失败（${rows} 行，其中文件 ${files} 个），请按 method=wiztree-walk 理解为目录遍历结果。topDirs=目录排行；topFiles=单个大文件；extAgg=按扩展名聚合（含文件数）；staleFiles=≥50MB 且 ≥1 年未修改的文件（大者优先）。全部只读统计。`,
		};
	} catch (e) {
		signal?.throwIfAborted();
		return { error: errorMessage(e).slice(0, 500) };
	} finally {
		try {
			rmSync(csvPath, { force: true });
		} catch {
			signal?.throwIfAborted();
			/* 清理失败不影响结果 */
		}
	}
}

/* ------------------------------------------------------------------ */
/* scope=usage · 回退路径：Node 逐文件 stat 累加                       */
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
	index: VolumeIndex,
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
			if (existsSync(WIZTREE)) {
				// 实测普通权限也能直读 MFT：先试 WizTree，失败/异常自动降级逐文件统计
				const fast = await usageViaWizTree(rootPath, topN, index, signal);
				if (fast && !fast.error) {
					result.usage = fast;
				} else {
					result.usage = await usageViaWalk(rootPath, topN, signal);
					if (fast?.error) asRecord(result.usage).degradedFrom = `wiztree: ${fast.error}`;
				}
			} else {
				result.usage = await usageViaWalk(rootPath, topN, signal);
			}
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
