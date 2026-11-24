import {
	Check,
	CircleAlert,
	CircleCheck,
	CircleX,
	Clock3,
	Copy,
	ExternalLink,
	FileTerminal,
	FileText,
	Globe,
	LoaderCircle,
	LockKeyhole,
	Monitor,
	Shield,
} from "lucide-react";
import { memo, useRef, useState } from "react";
import { RunningCorner } from "../conversation/running-light";
import { TranscriptDisclosure, type TranscriptFormat } from "../conversation/transcript-disclosure";
import { ApiFailure, apiJson, sessionPath } from "../data/api";
import { type PendingBitLocker, requestForCall } from "../data/bitlocker-state";
import type { Message } from "../data/web-state";
import { CodeBlock } from "../shell/code-block";
import { useCopyFeedback } from "../shell/use-copy-feedback";
import { BitLockerInput } from "./bitlocker-input";
import {
	mapSpecialTool,
	type SpecialStatus,
	type SpecialToolCall,
	type SpecialToolView,
	specialToolKind,
} from "./special-tool-data";

function StateIcon({ status, size = 12, title }: { status: SpecialStatus; size?: number; title?: string }) {
	const Icon =
		status === "running"
			? LoaderCircle
			: status === "pending" || status === "cancelled" || status === "interrupted"
				? Clock3
				: status === "success"
					? CircleCheck
					: status === "error"
						? CircleX
						: CircleAlert;
	const el = <Icon size={size} className={`special-tool-state special-tool-state--${status}`} aria-hidden="true" />;
	return title ? (
		<span className="special-tool-state-wrap" title={title}>
			{el}
		</span>
	) : (
		el
	);
}
function CopyButton({ value, label, compact = false }: { value: string; label: string; compact?: boolean }) {
	const { status, copy } = useCopyFeedback();
	const iconSize = compact ? 14 : 16;
	return (
		<>
			<button
				type="button"
				className={`btn-icon${compact ? " xs-24" : ""} special-tool-copy-btn`}
				aria-label={status === "copied" ? "已复制" : label}
				title={status === "copied" ? "已复制" : label}
				data-copied={status === "copied" || undefined}
				onClick={() => void copy(value)}
			>
				{status === "copied" ? (
					<Check size={iconSize} aria-hidden="true" />
				) : (
					<Copy size={iconSize} aria-hidden="true" />
				)}
			</button>
			<output className={status === "error" ? "special-tool-copy-error" : "visually-hidden"}>
				{status === "copied" ? "已复制" : status === "error" ? "复制失败，请手动选择复制" : ""}
			</output>
		</>
	);
}
function ToolDetails({
	view,
	call,
	request,
	format,
}: {
	view: SpecialToolView;
	call: SpecialToolCall;
	request?: PendingBitLocker;
	format?: TranscriptFormat;
}) {
	const [open, setOpen] = useState(false);
	const args = Object.entries(call.arguments)
		.filter(([key]) => key !== "recoveryPassword" && key !== "items")
		.map(([key, value]) => `${key}: ${JSON.stringify(value)}`)
		.join(", ");
	return (
		<TranscriptDisclosure
			format={format}
			className="special-tool-details"
			triggerClassName="special-tool-detail-trigger"
			heading="详情"
			indicatorSize={12}
			isExpanded={open}
			onExpandedChange={setOpen}
		>
			<div className="special-tool-detail-body">
				<code className="special-tool-invocation">
					{call.name}({args})
				</code>
				<dl>
					{view.fields.map((f, index) => (
						<div className="special-tool-field" key={`${f.label}-${index}`}>
							<dt>{f.label}</dt>
							<dd>{f.value}</dd>
						</div>
					))}
					{request?.targets.map((target) => (
						<div className="special-tool-field" key={target.volumeId}>
							<dt>固定目标</dt>
							<dd>
								{target.drive ?? "无盘符"} · {target.volumeId}
							</dd>
						</div>
					))}
				</dl>
				{view.errors.map((error, index) => (
					<p className="special-tool-detail-error" key={`${index}-${error}`}>
						<CircleAlert size={14} aria-hidden="true" />
						<span>{error}</span>
					</p>
				))}
				{view.raw && <CodeBlock className="special-tool-raw">{view.raw}</CodeBlock>}
			</div>
		</TranscriptDisclosure>
	);
}
function Overview({ view }: { view: Extract<SpecialToolView, { kind: "sys-overview" }> }) {
	return (
		<>
			<p className="special-tool-machine">{view.machine || "机型信息未获取"}</p>
			<div className="special-tool-meters">
				{[
					{
						label: "CPU",
						value: view.cpu,
						note: view.cores === null ? "逻辑核心未知" : `${view.cores} 个逻辑核心`,
					},
					{ label: "内存", value: view.memory, note: view.capacity },
				].map((meter) => (
					<section className="special-tool-meter" key={meter.label}>
						<div>
							<span>{meter.label}</span>
							<strong>{meter.value === null ? "未获取" : `${meter.value}%`}</strong>
						</div>
						{meter.value === null ? (
							<span className="special-tool-meter-empty" />
						) : (
							<progress
								aria-label={`${meter.label}占用`}
								value={Math.max(0, Math.min(100, meter.value))}
								max={100}
							/>
						)}
						<small>{meter.note}</small>
					</section>
				))}
			</div>
		</>
	);
}
function Runbook({
	view,
	sessionId,
	entryId,
	callId,
}: {
	view: Extract<SpecialToolView, { kind: "runbook" }>;
	sessionId?: string;
	entryId?: string;
	callId: string;
}) {
	const [busy, setBusy] = useState(false);
	const [feedback, setFeedback] = useState("");
	const key = useRef<string | undefined>(undefined);
	async function open() {
		if (!sessionId || !entryId || busy) return;
		setBusy(true);
		setFeedback("");
		key.current ??= crypto.randomUUID();
		try {
			await apiJson(sessionPath(sessionId, "/runbook/open"), {
				method: "POST",
				body: { entryId, toolCallId: callId },
				idempotencyKey: key.current,
			});
			setFeedback("已发起打开");
			key.current = undefined;
		} catch (cause) {
			if (cause instanceof ApiFailure && cause.code?.startsWith("runbook_")) {
				key.current = undefined;
				setFeedback(cause.message);
			} else if (cause instanceof ApiFailure && (cause.status === 400 || cause.status === 409)) {
				key.current = undefined;
				setFeedback("请核对清单和记事本。核对后可重新发起打开。");
			} else setFeedback("打开结果未确认。请核对记事本，再重试原请求。");
		} finally {
			setBusy(false);
		}
	}
	return (
		<>
			{view.status === "success" && (
				<div className="special-tool-runbook-meta">
					<span>{view.level}</span>
					<span>{view.count ?? "未知"} 条命令</span>
					{view.version !== null && <span>版本 {view.version}</span>}
					<span>{view.planned ? "待核对，尚未导出文本" : "手动执行"}</span>
				</div>
			)}
			<ol className="special-tool-commands">
				{view.commands.map((command, index) => (
					<li key={`${index}-${command.command}`}>
						<div className="special-tool-command-heading">
							<strong>{command.summary}</strong>
							<small>
								{command.shell === "powershell" ? "PowerShell" : command.shell} ·{" "}
								{command.admin ? "管理员" : "普通权限"}
							</small>
						</div>
						<div className="special-tool-command-line">
							<code>{command.command}</code>
							<CopyButton value={command.command} label={`复制第 ${index + 1} 条命令`} />
						</div>
					</li>
				))}
			</ol>
			{view.delivery && (
				<div className="special-tool-delivery">
					<FileText size={14} aria-hidden="true" />
					<button
						type="button"
						className="special-tool-file-link"
						disabled={!sessionId || !entryId || busy}
						onClick={() => void open()}
					>
						<span>{view.delivery.name}</span>
						<span>
							{busy ? "打开中" : "打开"}
							<ExternalLink size={12} aria-hidden="true" />
						</span>
					</button>
				</div>
			)}
			<output className={feedback ? "special-tool-feedback" : "visually-hidden"}>{feedback}</output>
		</>
	);
}
function BitLockerResult({ view }: { view: Extract<SpecialToolView, { kind: "bitlocker" }> }) {
	return (
		<div className="special-tool-targets">
			{view.volumes.map((volume, index) => (
				<div className="special-tool-target" key={`${volume.id}-${index}`}>
					<strong>{volume.drive}</strong>
					<div>
						<span>{view.scope === "unlock" ? volume.lock : volume.conversion}</span>
						<small>{view.scope === "unlock" ? volume.conversion : volume.lock}</small>
						{volume.reason && <small>{volume.reason}</small>}
						{view.scope === "decrypt" && volume.percentage !== null && (
							<>
								<small>加密比例 {volume.percentage}%</small>
								<progress aria-label={`${volume.drive}加密比例`} value={volume.percentage} max={100} />
							</>
						)}
					</div>
				</div>
			))}
		</div>
	);
}
export const SpecialTool = memo(function SpecialTool({
	call,
	result,
	live = false,
	sessionId,
	entryId,
	requests = [],
	format,
}: {
	call: SpecialToolCall;
	result?: Message;
	live?: boolean;
	sessionId?: string;
	entryId?: string;
	requests?: PendingBitLocker[];
	format?: TranscriptFormat;
}) {
	const [searchOpen, setSearchOpen] = useState(false);
	const request = requestForCall(requests, call.id, entryId);
	const [finishedRequest, setFinishedRequest] = useState<string>();
	const pending = request && finishedRequest !== request.requestId ? request : undefined;
	const view = mapSpecialTool(call, result, live || !!pending);
	if (!view) return null;
	const Icon =
		view.kind === "sys-overview"
			? Monitor
			: view.kind === "runbook"
				? FileTerminal
				: view.kind === "web-search"
					? Globe
					: view.scope === "unlock"
						? LockKeyhole
						: Shield;
	const status = pending ? "pending" : view.status;
	const summary = pending ? (pending.kind === "password" ? "等待输入" : "等待确认") : view.summary;
	if (view.kind === "web-search")
		return (
			<section className="special-tool special-tool--web-search" data-state={status} data-tool-call-id={call.id}>
				<RunningCorner active={status === "running" || status === "pending"} />
				<TranscriptDisclosure
					format={format}
					className="special-tool-search"
					triggerClassName="special-tool-search-trigger"
					heading={
						<>
							<Globe size={14} aria-hidden="true" />
							<span>{summary}</span>
							<StateIcon
								status={status}
								size={16}
								title={
									(status === "degraded" || status === "error") && view.errors[0] ? view.errors[0] : undefined
								}
							/>
						</>
					}
					suffix={<small>{view.queries.length} 个关键词</small>}
					indicatorSize={12}
					isExpanded={searchOpen}
					onExpandedChange={setSearchOpen}
				>
					<div>
						{view.queries.map((query, index) => (
							<p key={`${index}-${query}`}>{query}</p>
						))}
						{view.unstructured && !view.errors.length && view.raw && (
							<CodeBlock className="special-tool-raw">{view.raw}</CodeBlock>
						)}
						{view.errors.map((error, index) => (
							<p key={`${index}-${error}`} className="special-tool-detail-error">
								<CircleAlert size={14} aria-hidden="true" />
								<span>{error}</span>
							</p>
						))}
					</div>
				</TranscriptDisclosure>
			</section>
		);
	return (
		<section className={`special-tool special-tool--${view.kind}`} data-state={status} data-tool-call-id={call.id}>
			<RunningCorner active={status === "running" || status === "pending"} />
			<header className="special-tool-heading">
				<Icon size={18} aria-hidden="true" />
				<h3>{view.title}</h3>
				<output className="special-tool-status" key={status}>
					<StateIcon status={status} />
					<span>{summary}</span>
				</output>
			</header>
			{view.kind === "sys-overview" && view.status !== "error" && <Overview view={view} />}
			{view.kind === "runbook" && <Runbook view={view} sessionId={sessionId} entryId={entryId} callId={call.id} />}
			{view.kind === "bitlocker" && (
				<>
					{pending?.kind === "password" && (
						<div className="special-tool-keyids">
							{pending.targets
								.flatMap((t) => t.keyProtectors.filter((k) => k.type === 3))
								.map((key) => (
									<div key={key.id}>
										<small>密钥 ID</small>
										<span title={key.id}>{key.id}</span>
										<CopyButton value={key.id} label="复制恢复密钥 ID" compact />
									</div>
								))}
							{pending.targets.every((t) => !t.keyProtectors.some((k) => k.type === 3)) && (
								<small>密钥 ID 未获取</small>
							)}
						</div>
					)}
					{pending && sessionId && (
						<BitLockerInput
							key={pending.requestId}
							id={sessionId}
							request={pending}
							onDone={() => setFinishedRequest(pending.requestId)}
						/>
					)}
					{result && <BitLockerResult view={view} />}
				</>
			)}
			{(status === "error" || status === "unknown" || status === "degraded") && view.errors.length > 0 && (
				<p className="special-tool-result-error">
					<CircleAlert size={14} aria-hidden="true" />
					<span>{view.errors[0]}</span>
				</p>
			)}
			<footer className="special-tool-footer">
				{view.kind === "sys-overview" && view.status !== "error" && view.uptime && (
					<small>
						<Clock3 size={12} aria-hidden="true" />
						已运行 {view.uptime}
					</small>
				)}
				<ToolDetails view={view} call={call} request={pending} format={format} />
			</footer>
		</section>
	);
});
export const isSpecialTool = (call: SpecialToolCall) => specialToolKind(call) !== undefined;
