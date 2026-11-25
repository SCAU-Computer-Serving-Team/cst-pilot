import { Button, Tooltip } from "@heroui/react";
import { ArrowUp, CircleAlert, CircleHelp, GripVertical, Pause, Pencil, RotateCcw, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { apiJson, sessionPath } from "../data/api";
import type { InboxSnapshot } from "../data/web-state";
import { useWorkspaceShell } from "../shell/workspace-shell-context";
import { visibleQueueItems } from "./conversation-parts";

export const EDIT_QUEUE_ITEM_EVENT = "cst-pilot:edit-queue-item";

export type EditQueueItemDetail = {
	sessionId: string;
	itemId: string;
	version: number;
	text: string;
	skill?: string;
	images: InboxSnapshot["items"][number]["images"];
	refresh: () => Promise<void>;
};

export function QueuePanel({
	id,
	queue,
	disabled = false,
	refresh,
}: {
	id: string;
	queue: InboxSnapshot;
	disabled?: boolean;
	refresh: () => Promise<void>;
}) {
	const [error, setError] = useState("");
	const [busy, setBusy] = useState(false);
	const busyRef = useRef(false);
	const locked = busy || disabled;
	const [errorTipOpen, setErrorTipOpen] = useState(false);
	const shell = useWorkspaceShell();
	useEffect(() => {
		if (shell?.appActive) setErrorTipOpen(false);
	}, [shell?.appActive]);
	const pending = visibleQueueItems(queue.items);
	const failed = pending.find((item) => item.status === "failed");
	const uncertain = pending.some((item) => item.uncertain);
	const queuedIds = pending
		.filter((item) => item.status === "pending" && item.delivery === "queue")
		.map((item) => item.id);
	if (!pending.length) return null;
	async function change(path: string, method: "PATCH" | "DELETE" | "POST" | "PUT", body: object) {
		if (busyRef.current || disabled) return;
		busyRef.current = true;
		setBusy(true);
		try {
			await apiJson(sessionPath(id, `/${path}`), { method, body: { version: queue.version, ...body } });
			setError("");
			await refresh();
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "队列更新失败");
			await refresh();
		} finally {
			busyRef.current = false;
			setBusy(false);
		}
	}
	return (
		<section
			className="queue-panel"
			data-state={failed ? "failed" : uncertain ? "uncertain" : queue.paused ? "paused" : "queued"}
			aria-label="排队消息"
		>
			{failed ? (
				<output className="queue-status queue-status--failed">
					<span className="queue-status-left">
						<CircleAlert size={14} className="queue-status-icon" aria-hidden="true" />
						<span className="queue-status-title">投递失败</span>
						<Tooltip
							delay={80}
							closeDelay={0}
							isOpen={errorTipOpen && !shell?.appActive}
							onOpenChange={(next) => setErrorTipOpen(!shell?.appActive && next)}
						>
							<Tooltip.Trigger className="queue-error-info" aria-label="查看投递失败原因">
								<CircleHelp size={14} aria-hidden="true" />
							</Tooltip.Trigger>
							<Tooltip.Content className="tier-2-glass queue-error-tooltip" placement="top end">
								{failed.error || "消息未能投递，请检查模型服务。"}
							</Tooltip.Content>
						</Tooltip>
					</span>
				</output>
			) : uncertain ? (
				<output className="queue-status queue-status--warning">
					<span className="queue-status-left">
						<CircleAlert size={14} className="queue-status-icon" aria-hidden="true" />
						<span>上次投递结果待核对 · 请核对对话后再补充消息，不会自动重发</span>
					</span>
				</output>
			) : queue.paused ? (
				<output className="queue-status queue-status--paused">
					<span className="queue-status-left">
						<Pause size={14} className="queue-status-icon" aria-hidden="true" />
						<span>排队已暂停 · 发送新消息后继续（{pending.length} 条待发）</span>
					</span>
					<span className="queue-status-tag">paused</span>
				</output>
			) : (
				<div className="queue-status">
					<span className="queue-status-count">排队 · {pending.length}</span>
				</div>
			)}
			<ul className="queue-list">
				{pending.map((item) => {
					const draggable = item.status === "pending" && item.delivery === "queue";
					const encodedId = encodeURIComponent(item.id);
					return (
						<li
							className="queue-item"
							key={item.id}
							data-status={item.status}
							draggable={draggable && !locked}
							onDragStart={(event) => {
								event.dataTransfer.setData("text/plain", item.id);
								event.dataTransfer.effectAllowed = "move";
							}}
							onDragOver={(event) => {
								if (draggable && !locked) event.preventDefault();
							}}
							onDrop={(event) => {
								event.preventDefault();
								if (!draggable || busyRef.current || disabled) return;
								const source = event.dataTransfer.getData("text/plain");
								const ids = [...queuedIds];
								const from = ids.indexOf(source);
								const to = ids.indexOf(item.id);
								if (from < 0 || to < 0 || from === to) return;
								ids.splice(from, 1);
								ids.splice(to, 0, source);
								void change("queue/order", "PUT", { ids });
							}}
						>
							{draggable && <GripVertical size={14} aria-hidden="true" className="queue-grip" />}
							<span className="queue-item-text">{item.text || "图片消息"}</span>
							{(item.status === "pending" || item.status === "failed") && (
								<span className="queue-item-actions">
									{item.status === "failed" ? (
										<Tooltip delay={80} closeDelay={0} isDisabled={!!shell?.appActive}>
											<Button
												variant="ghost"
												isIconOnly
												isDisabled={locked}
												className="btn-icon xs-24 queue-action-retry"
												aria-label="重试排队消息"
												onPress={() => void change(`queue/${encodedId}/retry`, "POST", {})}
											>
												<RotateCcw size={14} aria-hidden="true" />
											</Button>
											<Tooltip.Content className="app-tooltip" placement="top">
												重试发送
											</Tooltip.Content>
										</Tooltip>
									) : (
										<Tooltip delay={80} closeDelay={0} isDisabled={!!shell?.appActive}>
											<Button
												variant="ghost"
												isIconOnly
												isDisabled={locked}
												className="btn-icon xs-24 queue-action-steer"
												aria-label="立即发送"
												onPress={() => void change(`queue/${encodedId}/steer`, "POST", {})}
											>
												<ArrowUp size={16} aria-hidden="true" />
											</Button>
											<Tooltip.Content className="app-tooltip" placement="top">
												立即发送
											</Tooltip.Content>
										</Tooltip>
									)}
									<Tooltip delay={80} closeDelay={0} isDisabled={!!shell?.appActive}>
										<Button
											variant="ghost"
											isIconOnly
											isDisabled={locked}
											className="btn-icon xs-24"
											aria-label="编辑排队消息"
											onPress={() => {
												window.dispatchEvent(
													new CustomEvent<EditQueueItemDetail>(EDIT_QUEUE_ITEM_EVENT, {
														detail: {
															sessionId: id,
															itemId: item.id,
															version: queue.version,
															text: item.text,
															skill: item.skill,
															images: item.images,
															refresh,
														},
													}),
												);
											}}
										>
											<Pencil size={14} aria-hidden="true" />
										</Button>
										<Tooltip.Content className="app-tooltip" placement="top">
											编辑
										</Tooltip.Content>
									</Tooltip>
									<Tooltip delay={80} closeDelay={0} isDisabled={!!shell?.appActive}>
										<Button
											variant="ghost"
											isIconOnly
											isDisabled={locked}
											className="btn-icon xs-24"
											aria-label="删除排队消息"
											onPress={() => void change(`queue/${encodedId}`, "DELETE", {})}
										>
											<Trash2 size={14} aria-hidden="true" />
										</Button>
										<Tooltip.Content className="app-tooltip" placement="top">
											删除
										</Tooltip.Content>
									</Tooltip>
								</span>
							)}
						</li>
					);
				})}
			</ul>
			{error && (
				<p role="alert" className="queue-error-text">
					{error} · 已重新读取队列，请重试。
				</p>
			)}
		</section>
	);
}
