import type { ContentPart, Message } from "../app/web-state";

type Call = Extract<ContentPart, { type: "toolCall" }>;
export type NoticeStatus = "default" | "warning" | "danger";
export type Block =
  | { kind: "fields"; id: string; title?: string; rows: { label: string; value: string }[] }
  | { kind: "listing"; id: string; items: { name: string; metrics: { label: string; value: string }[] }[] }
  | { kind: "text"; id: string; text: string }
  | { kind: "image"; id: string; mimeType: string; data: string }
  | { kind: "notice"; id: string; text: string; status: NoticeStatus }
  | { kind: "commands"; id: string; items: { shell: string; summary: string; command: string; admin: boolean }[] };
export type CardView = { status: "success" | "error" | "degraded"; blocks: Block[]; raw: string };

const record = (value: unknown): Record<string, unknown> | undefined => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
const words = (value: unknown): string => value == null ? "未知" : typeof value === "string" ? value : typeof value === "object" ? JSON.stringify(value) : String(value);
const booleans: Record<string, [string, string]> = { disabled: ["已禁用", "未禁用"], fetched: ["已取得正文", "未取得正文"], hasMore: ["有", "无"], admin: ["是", "否"], physical: ["是", "否"], found: ["是", "否"], truncated: ["是", "否"] };
const exactSuffixes: Record<string, string> = { n: " 次", files: " 个", hours: " 小时", pct: "%", C: " °C", W: " W" };
const endSuffixes: [string, string][] = [["Pct", "%"], ["KBs", " KB/s"], ["MBs", " MB/s"], ["GBs", " GB/s"], ["Sec", " 秒"], ["MB", " MB"], ["GB", " GB"]];
const measure = (key: string, value: unknown): string => {
  if (key in booleans) { const [yes, no] = booleans[key]; return value == null ? "状态未知" : value ? yes : no; }
  if (value == null) return "未知";
  if (typeof value === "boolean") return value ? "是" : "否";
  const suffix = exactSuffixes[key] ?? endSuffixes.find(([end]) => key.endsWith(end))?.[1] ?? "";
  return `${words(value)}${suffix}`;
};
// 容量换算为 GB，一位小数，整数不写 .0。
const gb = (mb: unknown) => (Number(mb) / 1024).toFixed(1).replace(/\.0$/, "");

const titles: Record<string, string> = {
  count: "数量", total: "总数", top: "返回条数", hours: "时间范围", method: "统计方式", path: "路径", size: "大小", bytes: "字节数", pct: "占比",
  name: "名称", type: "类型", status: "状态", text: "文本", note: "说明", category: "设备类", trim: "截断说明",
  space: "卷容量", drive: "卷标", label: "卷标", freeGB: "剩余容量", totalGB: "总容量", usedPct: "已使用", fileSystem: "文件系统", fs: "文件系统", driveType: "类型",
  physicalDisks: "物理硬盘", volumes: "卷信息", smart: "SMART 状态",
  FriendlyName: "型号", SerialNumber: "序列号", MediaType: "介质", BusType: "总线", HealthStatus: "健康", OperationalStatus: "运行状态", DeviceId: "设备 ID", deviceId: "设备 ID", sizeGB: "容量",
  root: "扫描根", filesScanned: "已扫描文件", elapsedSec: "耗时", budget: "扫描上限",
  topDirs: "目录排行", topFiles: "单文件排行", extAgg: "扩展名汇总", staleFiles: "旧文件",
  Wear: "磨损", PowerOnHours: "通电时长", ReadErrorsTotal: "读取错误", WriteErrorsTotal: "写入错误", ext: "扩展名", files: "文件数",
  cpuTotalPct: "CPU 总占用", logicalCores: "逻辑核心", mem: "内存", totalMB: "总量", usedMB: "已用", freeMB: "可用", usedGB: "已用",
  pagefile: "页面文件", allocMB: "分配", peakMB: "峰值", pool: "内核池", nonpagedMB: "非分页", pagedMB: "分页",
  machine: "机型", vendor: "厂商", model: "型号", cpu: "处理器", physicalCores: "物理核心", bios: "BIOS", biosDate: "BIOS 日期",
  totalProcs: "进程总数", cores: "核心数", intervalSec: "采样间隔",
  byCpu: "CPU 占用", byMem: "内存占用", wsMB: "工作集", cpuPct: "CPU 占用", pid: "进程 ID",
  disks: "磁盘活动", queueLen: "队列长度", busyPct: "忙碌度", readKBs: "读取", writeKBs: "写入", disk: "磁盘",
  byIo: "进程 I/O", ioKBs: "读写速率", byGpuPct: "GPU 占用", byDedicatedMB: "显存占用", gpuPct: "GPU 占用", dedicatedMB: "显存", engtypes: "引擎",
  adapters: "显示适配器", driver: "驱动", bus: "总线", nvidia: "NVIDIA 状态", powerW: "功耗", vramUsedMB: "显存已用", vramTotalMB: "显存总量", utilPct: "使用率",
  sensors: "传感器", hw: "硬件", sensorCount: "传感器数量", thermalZones: "热区", zone: "热区", passivePct: "被动散热", tempC: "温度",
  frequency: "频率", avgPctOfMax: "平均频率", minPctOfMax: "最低频率", admin: "管理员", pawnio: "PawnIO",
  devices: "设备", class: "设备类", errorCode: "错误码", hardwareIds: "硬件 ID", connId: "连接", physical: "物理设备", connStatus: "连接状态", removable: "可移动设备",
  services: "服务", state: "运行状态", drivers: "驱动", version: "版本", date: "日期", provider: "提供程序", net: "网络", bluetooth: "蓝牙", audio: "音频",
  events: "事件", time: "时间", logName: "日志", id: "事件 ID", levelName: "级别", recordId: "记录号", msg: "简述", counts: "出现次数", n: "次数", last: "最近一次", unreadable: "不可读",
  regItems: "注册表自启", startupFolders: "启动文件夹", source: "来源", command: "命令", disabled: "状态", scope: "范围", items: "条目", display: "显示名", collectionErrors: "采集错误",
  totalChildren: "直接子项总数", totalSize: "总大小", entries: "子项大小", unknownCount: "未知大小", omitted: "省略项", current: "当前目录",
  totalMatched: "命中条数", totalFiles: "索引文件", pageIndex: "当前页", hasMore: "有下一页", cursor: "游标", nextOffset: "下一偏移",
  file: "文件", sequence: "序号", levelLabel: "风险档", encoding: "文本格式", dir: "目录",
  query: "主张", claims: "主张判断", confidence: "可信度", rationale: "理由", sources: "来源", rank: "序号", title: "标题", url: "链接", quality: "质量", fetched: "取得正文",
  passages: "引文", source_url: "来源", source_rank: "来源序号", sourceCount: "来源数", passageCount: "引文数", responseId: "研究结果标识",
  urls: "来源", urlCount: "来源数", successful: "成功", mode: "模式", totalChars: "总字符", hasImage: "含图片",
  resultCount: "结果数", contentLength: "内容长度", offset: "当前偏移", returnedChars: "返回字符", findMode: "匹配模式", matchCount: "命中数", returnedMatches: "返回命中",
  truncation: "截断", truncatedBy: "截断方式", totalLines: "总行数", outputLines: "输出行数", error: "错误",
};

// 数组以排行组呈现；其余对象数组按每项一组概览组呈现。
const rankings = new Set(["topDirs", "topFiles", "extAgg", "staleFiles", "byCpu", "byMem", "disks", "byIo", "byGpuPct", "byDedicatedMB", "counts", "entries", "sources"]);
const joinParts = (parts: unknown[], fallback: string) => { const text = parts.filter((part) => part != null && part !== "").map(words).join(" · "); return text || fallback; };
const firstOf = (parts: unknown[], fallback: string) => { const value = parts.find((part) => part != null && part !== ""); return value == null ? fallback : words(value); };
// 画布给这些重复组单独加了分组标题，其余重复组不渲染标题。
const groupHeadings: Record<string, (entry: Record<string, unknown>, index: number) => string> = {
  space: (entry, index) => `卷 ${firstOf([entry.drive], String(index + 1))}`,
  volumes: (entry, index) => `卷 ${firstOf([entry.drive], String(index + 1))}`,
  physicalDisks: (_, index) => `物理盘 · ${index + 1}`,
  smart: (_, index) => `SMART 状态 · ${index + 1}`,
  devices: (_, index) => `设备 · ${index + 1}`,
  removable: (_, index) => `可移动设备 · ${index + 1}`,
  regItems: (entry, index) => `注册表自启 · ${firstOf([entry.name], String(index + 1))}`,
  startupFolders: (entry, index) => `启动文件夹 · ${firstOf([entry.scope], String(index + 1))}`,
  services: (entry, index) => `自启服务 · ${firstOf([entry.display, entry.name], String(index + 1))}`,
  events: (entry, index) => joinParts([entry.time, entry.logName], `事件 · ${index + 1}`),
  claims: (_, index) => `主张判断 · ${index + 1}`,
  sensors: (entry, index) => `传感器 · ${firstOf([entry.name], String(index + 1))}`,
  pagefile: (entry, index) => `页面文件 · ${firstOf([entry.name], String(index + 1))}`,
  thermalZones: (entry, index) => `热区 · ${firstOf([entry.zone], String(index + 1))}`,
  drivers: (entry, index) => `驱动 · ${firstOf([entry.device], String(index + 1))}`,
};
// 这些键缺值时不出字段行（画布用提示条或什么都不显示）。
const nullableKeys = new Set(["smart", "nvidia", "lhmGpu", "mem", "pool", "machine", "uptime", "frequency", "stats", "truncation", "omitted", "counts", "events", "sensors", "devices", "removable", "entries"]);
// 这些键在画布上渲染成提示条，不进字段行。
const noticeKeys = new Set(["notice", "infoNotice", "smartNotice", "degradedFrom", "error", "collectionErrors", "counterErrors", "smartErrors", "degraded", "outputTruncated", "truncated", "unreadable", "truncation", "omitted", "stats"]);
// 画布未展示的原始字段。
const hiddenKeys: Record<string, Set<string>> = {
  eventlog: new Set(["level", "found", "time", "logName"]),
  disk: new Set(["evt"]),
  fetch_content: new Set(["responseId"]),
  get_search_content: new Set(["responseId"]),
};
// 同名字段在不同工具里的叫法。
const toolTitles: Record<string, Record<string, string>> = {
  get_search_content: { type: "结果类型", offset: "当前偏移", returnedChars: "返回字符", nextOffset: "下一偏移" },
  eventlog: { machine: "机器" },
};
// 排行行的指标不展示这些字段（画布只列表名与关键指标）。
const metricSkips = new Set(["type", "pid", "engtypes"]);
const nameKeys = ["name", "path", "drive", "title", "provider", "disk", "ext", "hw", "zone", "scope", "FriendlyName", "display", "label"];
const diagnostics = new Set(["disk", "sys", "driver", "eventlog", "startup", "ls"]);
const textTools = new Set(["read", "ffgrep", "fffind", "fetch_content", "get_search_content"]);

export function mapTool(call: Call, result: Message): CardView {
  const raw = typeof result.content === "string" ? result.content : result.content.filter((part) => part.type === "text").map((part) => part.text).join("\n");
  const images = typeof result.content === "string" ? [] : result.content.filter((part): part is Extract<ContentPart, { type: "image" }> => part.type === "image");
  const detail = record((result as Message & { details?: unknown }).details);
  const scope = typeof call.arguments.scope === "string" ? call.arguments.scope : ({ sys: "overview", driver: "problem", eventlog: "recent" } as Record<string, string>)[call.name];
  const nested = scope && detail ? record(detail[scope]) : undefined;
  const source = diagnostics.has(call.name) && nested ? nested : detail;
  const hasUsefulData = source && Object.entries(source).some(([key, value]) => !noticeKeys.has(key) && value != null);
  const hasError = !!(detail?.error || source?.error);
  const partialError = !!source && Object.values(source).some((value) => !!record(value)?.error) || !!(source?.degradedFrom || source?.collectionErrors || source?.smartErrors || source?.counterErrors);
  const partiallySearched = typeof detail?.successfulQueries === "number" && typeof detail?.queryCount === "number" && detail.successfulQueries < detail.queryCount;
  const status: CardView["status"] = result.isError || (hasError && !hasUsefulData) ? "error" : hasError || partialError || partiallySearched || source?.degraded || detail?.degraded || (typeof detail?.successful === "number" && typeof detail?.urlCount === "number" && detail.successful < detail.urlCount) ? "degraded" : "success";

  const blocks: Block[] = [];
  const scopeTitles = call.name === "disk" && scope === "usage" ? { totalGB: "总大小" } : {};
  const label = (key: string) => toolTitles[call.name]?.[key] ?? scopeTitles[key as keyof typeof scopeTitles] ?? titles[key] ?? key;
  const push = (block: Block) => blocks.push(block);
  const notice = (id: string, text: string, status: NoticeStatus) => push({ kind: "notice", id, text, status });

  // 提示条：直接用工具返回的原文，不加键名前缀。
  const degradedText = (value: unknown) => `WizTree 不可用，已降级为逐文件统计（${words(value)}）。数值为下界。`;
  const errorText = (key: string, value: unknown) => {
    const list = Array.isArray(value) ? value : [value];
    return list.map((item) => {
      const entry = record(item);
      const body = entry ? entry.error ?? entry.message ?? entry.msg ?? entry : item;
      const scopeText = entry ? entry.deviceId ?? entry.name ?? entry.provider : undefined;
      return `${label(key)}：${scopeText == null ? "" : `${words(scopeText)} · `}${words(body)}`;
    });
  };
  const omittedText = (value: Record<string, unknown>) => [words(value.note), value.count != null ? `已省略 ${words(value.count)} 项` : "", value.size != null ? `合计 ${words(value.size)}` : "", value.unknownCount ? `其中 ${words(value.unknownCount)} 项大小未知` : ""].filter(Boolean).join("，").replace("，已省略", "。已省略") + "。";
  const truncationText = (value: Record<string, unknown>) => `${words(value.truncatedBy === "lines" ? "行" : value.truncatedBy)}数超出上限，输出 ${words(value.outputLines)} / ${words(value.totalLines)}。`;
  const pushNotices = (data: Record<string, unknown> | undefined) => {
    if (!data) return;
    const at = blocks.length;
    if (data.degradedFrom != null) notice(`degraded-${at}`, degradedText(data.degradedFrom), "warning");
    for (const key of ["smartNotice", "infoNotice", "notice", "outputNotice", "truncation", "omitted", "collectionErrors", "counterErrors", "smartErrors"]) {
      const value = data[key];
      if (value == null) continue;
      if (key === "truncation" && !record(value)?.truncated) continue;
      if (key === "omitted") { if (record(value)) notice(`omitted-${at}`, omittedText(record(value)!), "default"); continue; }
      if (key === "truncation") { notice(`truncation-${at}`, truncationText(record(value)!), "warning"); continue; }
      if (key === "collectionErrors" || key === "counterErrors" || key === "smartErrors") { for (const line of errorText(key, value)) notice(`${key}-${at}-${line.length}`, line, "warning"); continue; }
      notice(`${key}-${at}`, words(value), key === "notice" ? "default" : "warning");
    }
    if (data.outputTruncated || data.unreadable) notice(`cut-${at}`, "结果已截断，列表只代表本次返回的范围。", "warning");
  };

  const metricsOf = (key: string, entry: Record<string, unknown>) => {
    if (key === "counts") return [{ label: "次数", value: measure("n", entry.n) }, { label: "最近一次", value: measure("last", entry.last) }].filter((metric) => metric.value !== "未知");
    if (key === "sources") return [{ label: "序号", value: words(entry.rank) }, { label: "取得正文", value: measure("fetched", entry.fetched ?? false) }];
    return Object.entries(entry)
      .filter(([field]) => !nameKeys.includes(field) && !metricSkips.has(field) && field !== "ext" && (field !== "bytes" || entry.size == null))
      .map(([field, value]) => ({ label: label(field), value: measure(field, value) }));
  };
  const nameOf = (key: string, entry: Record<string, unknown>, index: number) => {
    if (key === "counts") return `${words(entry.provider ?? "未知来源")} / ${words(entry.id ?? "-")}`;
    for (const field of nameKeys) if (entry[field] != null) return words(entry[field]);
    return `${index + 1}`;
  };

  // 画布把同组的部分字段合并或改名，这里按组逐项处理。
  const entryRows = (key: string, entry: Record<string, unknown>, hidden: Set<string>) => {
    const rows = key === "startupFolders" ? [{ label: "路径", value: words(entry.path) }] : [];
    if (key === "startupFolders") for (const item of Array.isArray(entry.items) ? entry.items : []) {
      const file = record(item);
      if (file) rows.push({ label: words(file.name), value: measure("disabled", file.disabled) });
    }
    if (key === "startupFolders") return rows;
    const skip = new Set<string>(hidden);
    if (key === "space" || key === "volumes") skip.add("drive");
    if (key === "regItems") skip.add("name");
    if (key === "services") skip.add("display");
    return Object.entries(entry)
      .filter(([field]) => !noticeKeys.has(field) && !skip.has(field))
      .map(([field, value]) => ({ label: label(field), value: field === "label" && value == null ? "（无卷标）" : measure(field, value) }));
  };
  const appendGroup = (data: Record<string, unknown> | undefined, id: string, heading: string | undefined, depth = 0) => {
    if (!data || depth > 2) return;
    const start = blocks.length;
    const hidden = hiddenKeys[call.name] ?? new Set<string>();
    const rows: { label: string; value: string }[] = [];
    for (const [key, value] of Object.entries(data)) {
      if (noticeKeys.has(key) || hidden.has(key)) continue;
      if (call.name === "eventlog" && scope === "detail" && key === "msg" && typeof value === "string") { push({ kind: "text", id: `${id}-message`, text: value }); continue; }
      if (key === "stats" && record(value)) continue;
      if (Array.isArray(value)) {
        if (!value.length) { notice(`${id}-${key}-empty`, `${label(key)}：本次没有结果`, "default"); continue; }
        if (key === "passages") {
          value.forEach((item, index) => { const passage = record(item); const source = passage?.source_url; push({ kind: "text", id: `${id}-${key}-${index}`, text: source == null ? words(passage?.text) : `${words(passage?.text)}\n\n来源：${words(source)}` }); });
          continue;
        }
        if (rankings.has(key)) {
          const entries = value.map((item, index) => { const entry = record(item); return entry ? { name: nameOf(key, entry, index), metrics: metricsOf(key, entry) } : { name: `${index + 1}`, metrics: [{ label: "结果", value: words(item) }] }; });
          // 画布排行组不渲染分组标题。
          push({ kind: "listing", id: `${id}-${key}`, items: entries });
          continue;
        }
        value.forEach((item, index) => {
          const entry = record(item);
          if (!entry) { push({ kind: "text", id: `${id}-${key}-${index}`, text: words(item) }); return; }
          const rows = entryRows(key, entry, hidden);
          appendMergedRows(key, entry, rows);
          push({ kind: "fields", id: `${id}-${key}-${index}`, title: groupHeadings[key]?.(entry, index), rows });
        });
        continue;
      }
      if (key === "filesScanned" || key === "elapsedSec") continue;
      if (key === "pagefile" && Array.isArray(value)) continue;
      if (record(value) && depth < 2) { appendGroup(record(value), `${id}-${key}`, label(key), depth + 1); continue; }
      if (value === null && nullableKeys.has(key)) continue;
      if (typeof value !== "object" || value === null) rows.push({ label: label(key), value: measure(key, value) });
    }
    if (rows.length) blocks.splice(start, 0, { kind: "fields", id, title: heading, rows });
    if (data.stats && record(data.stats)) {
      const stats = record(data.stats)!;
      const target = blocks[blocks.findIndex((block) => block.kind === "fields" && block.id === id)];
      const row = { label: "扫描统计", value: `${words(stats.filesScanned)} 个文件 · 用时 ${words(stats.elapsedSec)}s` };
      if (target?.kind === "fields") target.rows.push(row);
      else blocks.splice(start, 0, { kind: "fields", id, title: heading, rows: [row] });
    }
  };
  // 画布把同组的剩余容量与已用比例合并成一行。
  const appendMergedRows = (_key: string, entry: Record<string, unknown>, rows: { label: string; value: string }[]) => {
    if (entry.freeGB == null && entry.usedPct == null) return;
    const free = rows.findIndex((row) => row.label === label("freeGB"));
    const used = rows.findIndex((row) => row.label === label("usedPct"));
    if (free < 0 && used < 0) return;
    const value = [free >= 0 ? measure("freeGB", entry.freeGB) : "", used >= 0 ? `已用 ${measure("usedPct", entry.usedPct)}` : ""].filter(Boolean).join(" · ");
    for (const index of [used, free].filter((at) => at >= 0).sort((a, b) => b - a)) rows.splice(index, 1);
    rows.push({ label: "剩余", value });
  };

  if (status === "error") {
    notice("error", words(detail?.error ?? source?.error ?? raw), "danger");
    return { status, blocks, raw };
  }
  if (call.name === "runbook") {
    const runbook = record(detail?.runbook);
    if (runbook) push({ kind: "fields", id: "runbook-file", rows: (["file", "sequence", "levelLabel", "items", "encoding"] as const).filter((key) => runbook[key] != null || key === "file").map((key) => ({ label: label(key), value: measure(key, runbook[key]) })) });
    const items = Array.isArray(call.arguments.items) ? call.arguments.items.map((item) => record(item)).filter((item): item is Record<string, unknown> => !!item) : [];
    if (items.length) push({ kind: "commands", id: "runbook-commands", items: items.map((item) => ({ shell: words(item.shell), summary: words(item.summary), command: words(item.command), admin: item.admin === true })) });
    pushNotices(runbook);
    pushNotices(detail);
  } else if (call.name === "source_check") {
    const artifact = record(detail?.artifact);
    const claims = Array.isArray(artifact?.claims) ? artifact.claims : [];
    if (claims.length) push({ kind: "fields", id: "claims", title: "主张判断", rows: claims.flatMap((item, index) => {
      const claim = record(item) ?? {};
      return [...(index === 0 && artifact?.query != null ? [{ label: "主张", value: words(artifact.query) }] : []), { label: "状态", value: words(claim.status) }, { label: "可信度", value: words(claim.confidence) }, { label: "理由", value: words(claim.rationale) }];
    }) });
    if (Array.isArray(artifact?.sources) && artifact.sources.length) appendGroup({ sources: artifact.sources }, "sources", undefined);
    if (Array.isArray(artifact?.passages) && artifact.passages.length) appendGroup({ passages: artifact.passages }, "passages", undefined);
    if (detail) push({ kind: "fields", id: "source-metadata", title: "继续查看", rows: (["responseId", "sourceCount", "passageCount"] as const).filter((key) => detail[key] != null).map((key) => ({ label: label(key), value: measure(key, detail[key]) })) });
    pushNotices(detail);
  } else if (textTools.has(call.name)) {
    if (detail) appendGroup(detail, "result", undefined);
    if (call.name === "read" && call.arguments.path) blocks.unshift({ kind: "fields", id: "read-path", rows: [{ label: "绝对路径", value: words(call.arguments.path) }] });
    if (raw && !(call.name === "read" && images.length && /^Read image file \[/.test(raw))) push({ kind: "text", id: "content", text: raw });
    if (call.name === "get_search_content" && detail?.truncated) notice("continue", `JSON 字符片段，非完整结论 · 已截断，续读 offset=${words(detail.nextOffset)}`, "warning");
    pushNotices(detail);
  } else if (call.name === "sys" && scope === "overview" && source) {
    const mem = record(source.mem);
    const machine = record(source.machine);
    const uptime = record(source.uptime);
    const pagefile = Array.isArray(source.pagefile) ? record(source.pagefile[0]) : undefined;
    const load = [
      source.cpuTotalPct == null ? null : { label: "CPU 总占用", value: measure("pct", source.cpuTotalPct) },
      mem?.usedPct == null ? null : { label: "内存占用", value: measure("pct", mem.usedPct) },
      uptime?.totalHours == null ? null : { label: "已开机", value: measure("hours", uptime.totalHours) },
    ].filter((row): row is { label: string; value: string } => !!row);
    if (load.length) push({ kind: "fields", id: "overview-load", rows: load });
    const system = [
      machine ? { label: "机型", value: `${words(machine.vendor)} ${words(machine.model)}`.trim() } : null,
      machine?.cpu == null ? null : { label: "处理器", value: words(machine.cpu) },
      pagefile ? { label: "页面文件", value: `${gb(pagefile.usedMB)} / ${gb(pagefile.allocMB)} GB` } : null,
    ].filter((row): row is { label: string; value: string } => !!row);
    if (system.length) push({ kind: "fields", id: "overview-system", title: "系统", rows: system });
    pushNotices(source);
    if (source !== detail) pushNotices(detail);
  } else if (diagnostics.has(call.name)) {
    if (call.name === "startup" && record(detail?.startup)) {
      const startup = record(detail?.startup)!;
      const summary: [string, string][] = [["注册表自启项", "regItems"], ["启动文件夹", "startupFolders"], ["自启服务", "services"]];
      push({ kind: "fields", id: "startup-summary", title: "自启概况", rows: summary.map(([text, key]) => ({ label: text, value: Array.isArray(startup[key]) ? String(startup[key].length) : "未知" })) });
      appendGroup(startup, "startup", undefined);
    } else appendGroup(source, scope ?? call.name, call.name === "ls" ? "目录概况" : undefined);
    if (source !== detail && detail) appendGroup(Object.fromEntries(Object.entries(detail).filter(([key]) => key.includes("Notice") || key === "degraded")), "collection", undefined);
    pushNotices(source);
    if (source !== detail) pushNotices(detail);
  } else {
    if (detail) appendGroup(detail, "result", undefined);
    if (raw) push({ kind: "text", id: "content", text: raw });
    pushNotices(detail);
  }
  const imageBlocks: Block[] = images.map((image, index) => ({ kind: "image", id: `image-${index}`, mimeType: image.mimeType, data: image.data }));
  if (call.name === "read") blocks.unshift(...imageBlocks);
  else blocks.push(...imageBlocks);
  if (!blocks.length) push({ kind: "text", id: "fallback", text: raw || "结果为空" });
  return { status, blocks, raw };
}
