import { Button, Input, Label, TextField, Tooltip } from "@heroui/react";
import { ArrowUp, CircleHelp, GripVertical, Pencil, Trash2 } from "lucide-react";
import { useState } from "react";
import { apiJson, sessionPath } from "../data/api";
import type { InboxSnapshot } from "../data/web-state";
import { visibleQueueItems } from "./conversation-parts";

export function QueuePanel({ id, queue, refresh }: { id: string; queue: InboxSnapshot; refresh: () => Promise<void> }) {
	const [error, setError] = useState("");
	const [editing, setEditing] = useState("");
	const [text, setText] = useState("");
	const pending = visibleQueueItems(queue.items);
	const failed = pending.find((item) => item.status === "failed");
	const queuedIds = pending
		.filter((item) => item.status === "pending" && item.delivery === "queue")
		.map((item) => item.id);
	if (!pending.length) return null;
	async function change(path: string, method: "PATCH" | "DELETE" | "POST" | "PUT", body: object) {
		try {
			await apiJson(sessionPath(id, `/${path}`), { method, body: { version: queue.version, ...body } });
			setError("");
			setEditing("");
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
						{editing === item.id ? (
							<>
								<TextField aria-label="编辑排队消息" className="queue-edit" value={text} onChange={setText}>
									<Label className="visually-hidden">编辑排队消息</Label>
									<Input className="queue-edit-input" />
								</TextField>
								<Button onPress={() => void change(`queue/${item.id}`, "PATCH", { text })}>保存</Button>
								<Button onPress={() => setEditing("")}>取消</Button>
							</>
						) : (
							<>
								<GripVertical size={16} aria-hidden="true" className="queue-grip" />
								<span>{item.text || "图片消息"}</span>
								{item.status === "pending" && (
									<>
										<Button
											variant="ghost"
											className="queue-steer"
											onPress={() => void change(`queue/${item.id}/steer`, "POST", {})}
										>
											<ArrowUp size={14} aria-hidden="true" />
											立即
										</Button>
										<Button
											variant="ghost"
											isIconOnly
											aria-label="编辑排队消息"
											onPress={() => {
												setEditing(item.id);
												setText(item.text);
											}}
										>
											<Pencil size={16} />
										</Button>
										<Button
											variant="ghost"
											isIconOnly
											aria-label="删除排队消息"
											onPress={() => void change(`queue/${item.id}`, "DELETE", {})}
										>
											<Trash2 size={16} />
										</Button>
									</>
								)}
							</>
						)}
					</li>
				))}
			</ul>
			{error && <p role="alert">{error} · 已重新读取队列，请重试。</p>}
		</section>
	);
}
