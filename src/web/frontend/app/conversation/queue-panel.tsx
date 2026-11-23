import { Button, Tooltip } from "@heroui/react";
import { ArrowUp, CircleHelp, GripVertical, Pencil, RotateCcw, Trash2 } from "lucide-react";
import { useState } from "react";
import { apiJson, sessionPath } from "../data/api";
import type { InboxSnapshot } from "../data/web-state";
import { visibleQueueItems } from "./conversation-parts";

export const EDIT_QUEUE_ITEM_EVENT = "cst-pilot:edit-queue-item";

export type EditQueueItemDetail = {
	sessionId: string;
	itemId: string;
	version: number;
	text: string;
	skill?: string;
	refresh: () => Promise<void>;
};

export function QueuePanel({ id, queue, refresh }: { id: string; queue: InboxSnapshot; refresh: () => Promise<void> }) {
	const [error, setError] = useState("");
	const pending = visibleQueueItems(queue.items);
	const failed = pending.find((item) => item.status === "failed");
	const uncertain = pending.some((item) => item.uncertain);
	const queuedIds = pending
		.filter((item) => item.status === "pending" && item.delivery === "queue")
		.map((item) => item.id);
	if (!pending.length) return null;
	async function change(path: string, method: "PATCH" | "DELETE" | "POST" | "PUT", body: object) {
		try {
			await apiJson(sessionPath(id, `/${path}`), { method, body: { version: queue.version, ...body } });
			setError("");
			await refresh();
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "队列更新失败");
			await refresh();
		}
	}
	return (
		<section className="queue-panel" aria-label="排队消息">
			{failed ? (
				<output className="queue-status">
					<span>投递失败</span>
					<Tooltip delay={80} closeDelay={0}>
						<Tooltip.Trigger className="queue-error-info" aria-label="查看投递失败原因">
							<CircleHelp size={16} aria-hidden="true" />
						</Tooltip.Trigger>
						<Tooltip.Content className="queue-error-tooltip" placement="top end">
							{failed.error || "消息未能投递，请检查模型服务。"}
						</Tooltip.Content>
					</Tooltip>
				</output>
			) : uncertain ? (
				<output className="queue-status">上次投递结果待核对 · 请核对对话后再补充消息，不会自动重发</output>
			) : queue.paused ? (
				<strong className="queue-status">排队已暂停 · 发送新消息后继续</strong>
			) : null}
			<ul className="queue-list">
				{pending.map((item) => (
					<li
						className="queue-item"
						key={item.id}
						draggable={item.status === "pending" && item.delivery === "queue"}
						onDragStart={(event) => {
							event.dataTransfer.setData("text/plain", item.id);
							event.dataTransfer.effectAllowed = "move";
						}}
						onDragOver={(event) => {
							if (item.status === "pending" && item.delivery === "queue") event.preventDefault();
						}}
						onDrop={(event) => {
							event.preventDefault();
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
						{!item.uncertain && <GripVertical size={16} aria-hidden="true" className="queue-grip" />}
						<span>{item.text || "图片消息"}</span>
						{(item.status === "pending" || item.status === "failed") && (
							<>
								{item.status === "failed" ? (
									<Tooltip delay={80} closeDelay={0}>
										<Button
											variant="ghost"
											isIconOnly
											aria-label="重试排队消息"
											onPress={() => void change(`queue/${item.id}/retry`, "POST", {})}
										>
											<RotateCcw size={16} />
										</Button>
										<Tooltip.Content placement="top">重试发送</Tooltip.Content>
									</Tooltip>
								) : (
									<Tooltip delay={80} closeDelay={0}>
										<Button
											variant="ghost"
											isIconOnly
											aria-label="立即发送"
											onPress={() => void change(`queue/${item.id}/steer`, "POST", {})}
										>
											<ArrowUp size={16} />
										</Button>
										<Tooltip.Content placement="top">立即发送</Tooltip.Content>
									</Tooltip>
								)}
								<Tooltip delay={80} closeDelay={0}>
									<Button
										variant="ghost"
										isIconOnly
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
														refresh,
													},
												}),
											);
										}}
									>
										<Pencil size={16} />
									</Button>
									<Tooltip.Content placement="top">编辑</Tooltip.Content>
								</Tooltip>
								<Tooltip delay={80} closeDelay={0}>
									<Button
										variant="ghost"
										isIconOnly
										aria-label="删除排队消息"
										onPress={() => void change(`queue/${item.id}`, "DELETE", {})}
									>
										<Trash2 size={16} />
									</Button>
									<Tooltip.Content placement="top">删除</Tooltip.Content>
								</Tooltip>
							</>
						)}
					</li>
				))}
			</ul>
			{error && <p role="alert">{error} · 已重新读取队列，请重试。</p>}
		</section>
	);
}
