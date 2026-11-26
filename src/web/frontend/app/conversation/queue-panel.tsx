import { Button, Tooltip } from "@heroui/react";
import { ArrowUp, CircleAlert, CircleHelp, GripVertical, Pause, Pencil, RotateCcw, Trash2 } from "lucide-react";
import { type KeyboardEvent as ReactKeyboardEvent, useCallback, useEffect, useRef, useState } from "react";
import { apiJson, sessionPath } from "../data/api";
import type { InboxSnapshot } from "../data/web-state";
import { KbdFocusOverlay, type KeyboardFocusTone, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
import { resolveListNavKey, resolveToolbarNavKey } from "../shell/keyboard-navigation";
import { useWorkspaceShell } from "../shell/workspace-shell-context";
import { visibleQueueItems } from "./conversation-parts";
import { acceptNewerQueue, reconcilePendingOrder } from "./queue-reorder";

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
	const [confirmedQueue, setConfirmedQueue] = useState<InboxSnapshot>(queue);
	const confirmedSessionRef = useRef(id);
	const confirmedQueueRef = useRef<InboxSnapshot>(queue);
	if (confirmedSessionRef.current !== id) {
		confirmedSessionRef.current = id;
		confirmedQueueRef.current = queue;
		if (confirmedQueue !== queue) setConfirmedQueue(queue);
	} else {
		const merged = acceptNewerQueue(confirmedQueueRef.current, queue);
		if (merged !== confirmedQueueRef.current) {
			confirmedQueueRef.current = merged;
			if (confirmedQueue !== merged) setConfirmedQueue(merged);
		}
	}
	const [optimisticOrder, setOptimisticOrder] = useState<string[] | null>(null);
	const pendingOrderRef = useRef<string[] | null>(null);
	const listRef = useRef<HTMLDivElement>(null);
	const itemRefs = useRef<Map<string, HTMLElement>>(new Map());
	const kbdStyle = useKeyboardFocusStyle();
	const [kbdActive, setKbdActive] = useState(false);
	const [activeRowId, setActiveRowId] = useState<string | null>(null);
	const [activeTargetId, setActiveTargetId] = useState<string | null>(null);
	const [activeTone, setActiveTone] = useState<KeyboardFocusTone>("accent");
	const clearKbd = useCallback(() => setKbdActive(false), []);
	usePointerMutex(kbdActive, clearKbd);
	const shell = useWorkspaceShell();
	useEffect(() => {
		if (shell?.appActive) setErrorTipOpen(false);
	}, [shell?.appActive]);
	const effectiveQueue = confirmedQueue;
	const rawPending = visibleQueueItems(effectiveQueue.items);
	const pending = (() => {
		if (!optimisticOrder) return rawPending;
		const byId = new Map(rawPending.map((item) => [item.id, item]));
		const reordered = optimisticOrder
			.map((itemId) => byId.get(itemId))
			.filter((item): item is (typeof rawPending)[number] => Boolean(item));
		let slot = 0;
		return rawPending.map((item) => {
			if (item.status === "pending" && item.delivery === "queue" && slot < reordered.length) {
				return reordered[slot++];
			}
			return item;
		});
	})();
	const failed = pending.find((item) => item.status === "failed");
	const uncertain = pending.some((item) => item.uncertain);
	const queuedIds = pending
		.filter((item) => item.status === "pending" && item.delivery === "queue")
		.map((item) => item.id);
	if (!pending.length) return null;
	async function flushReorder(initialIds: string[]) {
		if (disabled) return;
		if (busyRef.current) {
			pendingOrderRef.current = initialIds;
			setOptimisticOrder(initialIds);
			return;
		}
		busyRef.current = true;
		setBusy(true);
		pendingOrderRef.current = initialIds;
		try {
			while (pendingOrderRef.current) {
				const desired = pendingOrderRef.current;
				pendingOrderRef.current = null;
				const reconciled = reconcilePendingOrder(confirmedQueueRef.current, desired);
				if (!reconciled) {
					setOptimisticOrder(null);
					continue;
				}
				setOptimisticOrder(reconciled);
				const res = await apiJson<{ queue?: InboxSnapshot }>(sessionPath(id, "/queue/order"), {
					method: "PUT",
					body: { version: confirmedQueueRef.current.version, ids: reconciled },
				});
				const accepted = acceptNewerQueue(confirmedQueueRef.current, res?.queue);
				if (accepted !== confirmedQueueRef.current) {
					confirmedQueueRef.current = accepted;
					setConfirmedQueue(accepted);
				}
				setError("");
				if (!pendingOrderRef.current) {
					await refresh();
					// Re-check if another rapid Alt+Up/Down arrived during refresh!
					if (!pendingOrderRef.current) {
						setOptimisticOrder(null);
					}
				}
			}
		} catch (cause) {
			pendingOrderRef.current = null;
			setOptimisticOrder(null);
			setError(cause instanceof Error ? cause.message : "队列更新失败");
			await refresh();
		} finally {
			busyRef.current = false;
			setBusy(false);
		}
	}
	async function change(path: string, method: "PATCH" | "DELETE" | "POST" | "PUT", body: object) {
		if (busyRef.current || disabled) return;
		busyRef.current = true;
		setBusy(true);
		try {
			const res = await apiJson<{ queue?: InboxSnapshot }>(sessionPath(id, `/${path}`), {
				method,
				body: { version: confirmedQueueRef.current.version, ...body },
			});
			const accepted = acceptNewerQueue(confirmedQueueRef.current, res?.queue);
			if (accepted !== confirmedQueueRef.current) {
				confirmedQueueRef.current = accepted;
				setConfirmedQueue(accepted);
			}
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
	function handleRowKeyDown(
		event: ReactKeyboardEvent<HTMLDivElement>,
		itemIndex: number,
		itemId: string,
		draggable: boolean,
	) {
		if (event.nativeEvent.isComposing || event.keyCode === 229) return;
		if (
			event.altKey &&
			!event.ctrlKey &&
			!event.metaKey &&
			!event.shiftKey &&
			(event.key === "ArrowUp" || event.key === "ArrowDown")
		) {
			if (!draggable || disabled) return;
			const pos = queuedIds.indexOf(itemId);
			const nextPos = event.key === "ArrowUp" ? pos - 1 : pos + 1;
			if (pos < 0 || nextPos < 0 || nextPos >= queuedIds.length) return;
			event.preventDefault();
			const ids = [...queuedIds];
			[ids[pos], ids[nextPos]] = [ids[nextPos], ids[pos]];
			setKbdActive(true);
			setActiveRowId(itemId);
			setActiveTargetId(`row:${itemId}`);
			setActiveTone("accent");
			void flushReorder(ids);
			return;
		}
		if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
			const buttons = Array.from(
				event.currentTarget.querySelectorAll<HTMLButtonElement>(".queue-item-actions button:not(:disabled)"),
			);
			if (!buttons.length) return;
			const focusedIdx = buttons.indexOf(document.activeElement as HTMLButtonElement);
			if (focusedIdx < 0) {
				if (event.key === "ArrowRight") {
					event.preventDefault();
					setKbdActive(true);
					buttons[0]?.focus({ preventScroll: true });
				}
				return;
			}
			const nextBtn = resolveToolbarNavKey(
				{
					key: event.key,
					defaultPrevented: event.defaultPrevented,
					isComposing: event.nativeEvent.isComposing,
					keyCode: event.keyCode,
					ctrlKey: event.ctrlKey,
					metaKey: event.metaKey,
					altKey: event.altKey,
					shiftKey: event.shiftKey,
				},
				focusedIdx,
				buttons.length,
			);
			if (nextBtn !== null) {
				event.preventDefault();
				setKbdActive(true);
				if (event.key === "ArrowLeft" && focusedIdx === 0) {
					setActiveRowId(itemId);
					setActiveTargetId(`row:${itemId}`);
					setActiveTone("accent");
					event.currentTarget.focus({ preventScroll: true });
				} else {
					buttons[nextBtn]?.focus({ preventScroll: true });
				}
			}
			return;
		}
		const nextRow = resolveListNavKey(
			{
				key: event.key,
				defaultPrevented: event.defaultPrevented,
				isComposing: event.nativeEvent.isComposing,
				keyCode: event.keyCode,
				ctrlKey: event.ctrlKey,
				metaKey: event.metaKey,
				altKey: event.altKey,
				shiftKey: event.shiftKey,
			},
			itemIndex,
			pending.length,
		);
		if (nextRow !== null && nextRow !== itemIndex) {
			event.preventDefault();
			const target = pending[nextRow];
			if (target) {
				setKbdActive(true);
				setActiveRowId(target.id);
				setActiveTargetId(`row:${target.id}`);
				setActiveTone("accent");
				itemRefs.current.get(`row:${target.id}`)?.focus({ preventScroll: true });
			}
		}
	}
	const validTargetId =
		kbdActive && activeTargetId && itemRefs.current.has(activeTargetId) ? activeTargetId : null;
	return (
		<section
			className="queue-panel"
			data-state={failed ? "failed" : uncertain ? "uncertain" : effectiveQueue.paused ? "paused" : "queued"}
			data-kbd-active={validTargetId ? "true" : undefined}
			aria-label="排队消息"
			onBlur={(event) => {
				if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
					setKbdActive(false);
					setActiveTargetId(null);
					setActiveTone("accent");
				}
			}}
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
			<div
				ref={listRef}
				role="listbox"
				aria-label="排队消息列表"
				className="queue-list"
				style={{ position: "relative" }}
			>
				{pending.map((item, itemIndex) => {
					const draggable = item.status === "pending" && item.delivery === "queue";
					const encodedId = encodeURIComponent(item.id);
					const rowKey = `row:${item.id}`;
					const retryKey = `act:${item.id}:retry`;
					const steerKey = `act:${item.id}:steer`;
					const editKey = `act:${item.id}:edit`;
					const deleteKey = `act:${item.id}:delete`;
					return (
						<div
							ref={(node) => {
								if (node) itemRefs.current.set(rowKey, node);
								else itemRefs.current.delete(rowKey);
							}}
							className="queue-item"
							key={item.id}
							role="option"
							aria-selected={activeRowId === item.id}
							tabIndex={0}
							data-status={item.status}
							draggable={draggable && !locked}
							onFocus={(event) => {
								if (event.target !== event.currentTarget) return;
								setActiveRowId(item.id);
								setActiveTargetId(rowKey);
								setActiveTone("accent");
								if (event.currentTarget.matches(":focus-visible")) setKbdActive(true);
							}}
							onKeyDown={(event) => handleRowKeyDown(event, itemIndex, item.id, draggable)}
							onDragStart={(event) => {
								event.dataTransfer.setData("text/plain", item.id);
								event.dataTransfer.effectAllowed = "move";
							}}
							onDragOver={(event) => {
								if (draggable && !locked) event.preventDefault();
							}}
							onDrop={(event) => {
								event.preventDefault();
								if (!draggable || disabled) return;
								const source = event.dataTransfer.getData("text/plain");
								const ids = [...queuedIds];
								const from = ids.indexOf(source);
								const to = ids.indexOf(item.id);
								if (from < 0 || to < 0 || from === to) return;
								ids.splice(from, 1);
								ids.splice(to, 0, source);
								void flushReorder(ids);
							}}
						>
							{draggable && <GripVertical size={14} aria-hidden="true" className="queue-grip" />}
							<span className="queue-item-text">{item.text || "图片消息"}</span>
							{(item.status === "pending" || item.status === "failed") && (
								<span className="queue-item-actions">
									{item.status === "failed" ? (
										<Tooltip delay={80} closeDelay={0} isDisabled={!!shell?.appActive}>
											<Button
												ref={(node) => {
													if (node) itemRefs.current.set(retryKey, node);
													else itemRefs.current.delete(retryKey);
												}}
												variant="ghost"
												isIconOnly
												isDisabled={locked}
												className="btn-icon xs-24 queue-action-retry"
												aria-label="重试排队消息"
												onFocus={(event) => {
													setActiveRowId(item.id);
													setActiveTargetId(retryKey);
													setActiveTone("accent");
													if (event.currentTarget.matches(":focus-visible")) setKbdActive(true);
												}}
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
												ref={(node) => {
													if (node) itemRefs.current.set(steerKey, node);
													else itemRefs.current.delete(steerKey);
												}}
												variant="ghost"
												isIconOnly
												isDisabled={locked}
												className="btn-icon xs-24 queue-action-steer"
												aria-label="立即发送"
												onFocus={(event) => {
													setActiveRowId(item.id);
													setActiveTargetId(steerKey);
													setActiveTone("accent");
													if (event.currentTarget.matches(":focus-visible")) setKbdActive(true);
												}}
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
											ref={(node) => {
												if (node) itemRefs.current.set(editKey, node);
												else itemRefs.current.delete(editKey);
											}}
											variant="ghost"
											isIconOnly
											isDisabled={locked}
											className="btn-icon xs-24"
											aria-label="编辑排队消息"
											onFocus={(event) => {
												setActiveRowId(item.id);
												setActiveTargetId(editKey);
												setActiveTone("accent");
												if (event.currentTarget.matches(":focus-visible")) setKbdActive(true);
											}}
											onPress={() => {
												window.dispatchEvent(
													new CustomEvent<EditQueueItemDetail>(EDIT_QUEUE_ITEM_EVENT, {
														detail: {
															sessionId: id,
															itemId: item.id,
															version: confirmedQueueRef.current.version,
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
											ref={(node) => {
												if (node) itemRefs.current.set(deleteKey, node);
												else itemRefs.current.delete(deleteKey);
											}}
											variant="ghost"
											isIconOnly
											isDisabled={locked}
											className="btn-icon xs-24"
											aria-label="删除排队消息"
											onFocus={(event) => {
												setActiveRowId(item.id);
												setActiveTargetId(deleteKey);
												setActiveTone("danger");
												if (event.currentTarget.matches(":focus-visible")) setKbdActive(true);
											}}
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
						</div>
					);
				})}
				<KbdFocusOverlay
					as="div"
					containerRef={listRef}
					itemRefs={itemRefs}
					targetId={validTargetId}
					style={kbdStyle}
					tone={activeTone}
					railPlacement="inside"
				/>
			</div>
			{error && (
				<p role="alert" className="queue-error-text">
					{error} · 已重新读取队列，请重试。
				</p>
			)}
		</section>
	);
}
