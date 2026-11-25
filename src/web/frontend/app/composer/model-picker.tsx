import { Button, ListBox, Popover } from "@heroui/react";
import { Check, ChevronDown, ChevronLeft, ChevronRight } from "lucide-react";
import { type CSSProperties, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ModelChoice } from "../data/model-selection";
import type { Model } from "../data/web-state";
import { usePanelDismiss } from "../shell/panel-dismiss";
import { useWorkspaceShell } from "../shell/workspace-shell-context";
import {
	computeModelPickerHeight,
	computeModelPickerWidth,
	type ModelPickerPage,
	modelDisplayName,
} from "./model-picker-size";

const fullId = (model: Model) => `${model.provider}/${model.id}`;
function thinkingLabel(level = "off") {
	return level === "xhigh" || level === "max" ? "Max" : level.charAt(0).toUpperCase() + level.slice(1);
}
type Page = ModelPickerPage;

export function ModelPicker({
	selected,
	models,
	thinking,
	levels,
	onOpen,
	onSelect,
	onThinking,
}: {
	selected: ModelChoice | null;
	models: Model[];
	thinking: string;
	levels: string[];
	onOpen: () => void;
	onSelect: (key: string) => Promise<boolean>;
	onThinking: (level: string) => Promise<boolean>;
}) {
	const [open, setOpen] = useState(false);
	const [page, setPage] = useState<Page>("main");
	const [listPage, setListPage] = useState<Exclude<Page, "main">>("models");
	const [busy, setBusy] = useState(false);
	const pending = useRef(false);
	const navigation = useRef(0);
	const openRef = useRef(open);
	openRef.current = open;
	const pageRef = useRef<Page>(page);
	pageRef.current = page;
	const escapeSubpageGuard = useRef(false);
	const returnFocus = useRef<Exclude<Page, "main"> | null>(null);
	const triggerButton = useRef<HTMLButtonElement>(null);
	const modelButton = useRef<HTMLButtonElement>(null);
	const thinkingButton = useRef<HTMLButtonElement>(null);
	const listBox = useRef<HTMLDivElement>(null);
	const shell = useWorkspaceShell();
	useEffect(() => {
		if (shell?.appActive) {
			navigation.current++;
			setOpen(false);
		}
	}, [shell?.appActive]);
	usePanelDismiss(
		open,
		() => {
			navigation.current++;
			setOpen(false);
		},
		triggerButton,
	);
	// 在组件首次挂载时抢先于 Popover 子树注册捕获监听，保证子列表 Esc 只退回主菜单，主菜单 Esc 归还入口焦点。
	useLayoutEffect(() => {
		function onEscape(event: KeyboardEvent) {
			if (!openRef.current || event.key !== "Escape") return;
			// 输入法组合态下的 Esc 只应交给输入法，拦截传播以阻止第三方 Popover 误关闭，且不调用 preventDefault。
			if (event.isComposing || event.keyCode === 229) {
				escapeSubpageGuard.current = true;
				queueMicrotask(() => {
					escapeSubpageGuard.current = false;
				});
				event.stopPropagation();
				event.stopImmediatePropagation();
				return;
			}
			if (event.defaultPrevented) return;
			event.preventDefault();
			event.stopPropagation();
			event.stopImmediatePropagation();
			if (pageRef.current !== "main") {
				escapeSubpageGuard.current = true;
				queueMicrotask(() => {
					escapeSubpageGuard.current = false;
				});
				returnFocus.current = pageRef.current === "models" ? "models" : "thinking";
				navigation.current++;
				setPage("main");
				return;
			}
			navigation.current++;
			setOpen(false);
			window.setTimeout(() => triggerButton.current?.focus(), 0);
		}
		window.addEventListener("keydown", onEscape, true);
		return () => window.removeEventListener("keydown", onEscape, true);
	}, []);
	useLayoutEffect(() => {
		if (!open || page === "main" || busy) return;
		function focusList() {
			const option = listBox.current?.querySelector<HTMLElement>('[role="option"][tabindex="0"]');
			(option ?? listBox.current)?.focus({ preventScroll: true });
		}
		focusList();
		const frame = requestAnimationFrame(focusList);
		return () => cancelAnimationFrame(frame);
	}, [open, page, busy]);
	useLayoutEffect(() => {
		if (open && page === "main" && returnFocus.current) {
			(returnFocus.current === "models" ? modelButton : thinkingButton).current?.focus();
			returnFocus.current = null;
		}
	}, [open, page]);
	function back() {
		returnFocus.current = page === "main" ? null : page;
		navigation.current++;
		setPage("main");
	}
	async function choose(key: string) {
		if (pending.current) return;
		const current = page === "models" ? modelKey : thinking;
		if (key === current) {
			back();
			return;
		}
		const revision = navigation.current;
		pending.current = true;
		setBusy(true);
		try {
			const success = await (page === "models" ? onSelect(key) : onThinking(key));
			if (success && revision === navigation.current) back();
		} finally {
			pending.current = false;
			setBusy(false);
		}
	}
	const modelKey = selected ? fullId(selected) : null;
	const selectedLabel = selected ? modelDisplayName(selected) : "选择模型";
	const supportedLevels = levels.length ? levels : ["off"];
	const thinkingLabels = supportedLevels.map((level) => thinkingLabel(level));
	const options =
		listPage === "models"
			? models.map((model) => {
					const label = modelDisplayName(model);
					return {
						id: fullId(model),
						label,
						provider: model.provider,
						title: `${model.provider} · ${label}`,
					};
				})
			: supportedLevels.map((level) => {
					const label = thinkingLabel(level);
					return { id: level, label, provider: undefined, title: label };
				});
	const selectedKey = listPage === "models" ? modelKey : thinking;
	// 宽度与桌面端对齐（224–320px 自适应，窄视口受 calc(100vw - 32px) 限制），高度按行数连续过渡。
	const pickerHeight = computeModelPickerHeight(page, options.length);
	const pickerWidth = computeModelPickerWidth({
		page,
		selectedLabel,
		thinkingLabel: thinkingLabel(thinking),
		models,
		thinkingLabels,
	});
	return (
		<Popover
			isOpen={open}
			onOpenChange={(next) => {
				if (!next && escapeSubpageGuard.current) return;
				navigation.current++;
				setOpen(next);
				if (next) {
					returnFocus.current = null;
					setPage("main");
					onOpen();
				}
			}}
		>
			<Button
				ref={triggerButton}
				variant="ghost"
				className="btn-ghost composer-model"
				data-open={open || undefined}
				aria-label={`模型与思考强度：${selected ? selectedLabel : "加载中"}，${thinkingLabel(thinking)}`}
				isDisabled={!selected && !models.length}
			>
				<span className="model-name" title={selected ? selectedLabel : undefined}>
					{selectedLabel}
				</span>
				<span className="model-current-strength">{thinkingLabel(thinking)}</span>
				<span className="model-chevron">
					<ChevronDown size={12} aria-hidden="true" />
				</span>
			</Button>
			<Popover.Content
				className={`tier-2-glass composer-popover model-picker-popover t-resize ${page === "main" ? "" : "model-picker-list"}`}
				data-page={page}
				style={{ "--picker-height": `${pickerHeight}px`, "--picker-width": `${pickerWidth}px` } as CSSProperties}
				placement="top end"
				offset={8}
				containerPadding={16}
			>
				<Popover.Dialog
					aria-label={page === "models" ? "选择模型" : page === "thinking" ? "选择思考强度" : "模型与思考强度"}
					className="model-picker-dialog"
					aria-busy={busy}
					data-busy={busy}
					data-page={page}
				>
					<div className="model-picker-pages t-page-slide" data-page={page === "main" ? "1" : "2"}>
						<div
							className="model-picker-main t-page"
							data-page-id="1"
							aria-hidden={page !== "main"}
							inert={page !== "main"}
						>
							<Button
								ref={modelButton}
								variant="ghost"
								className="menu-row model-picker-model"
								aria-label="选择模型"
								isDisabled={!models.length}
								onPress={() => {
									navigation.current++;
									setListPage("models");
									setPage("models");
								}}
							>
								<span className="model-picker-row-label">模型</span>
								<span
									className="model-picker-row-value"
									title={selected ? `${selected.provider} · ${selectedLabel}` : undefined}
								>
									<span className="model-picker-row-text">{selectedLabel}</span>
									<ChevronRight size={14} aria-hidden="true" />
								</span>
							</Button>
							<Button
								ref={thinkingButton}
								variant="ghost"
								className="menu-row model-picker-thinking"
								aria-label="选择思考强度"
								onPress={() => {
									navigation.current++;
									setListPage("thinking");
									setPage("thinking");
								}}
							>
								<span className="model-picker-row-label">思考强度</span>
								<span className="model-picker-row-value">
									<span className="model-picker-row-text">{thinkingLabel(thinking)}</span>
									<ChevronRight size={14} aria-hidden="true" />
								</span>
							</Button>
						</div>
						<div
							className="model-picker-options t-page"
							data-page-id="2"
							aria-hidden={page === "main"}
							inert={page === "main"}
						>
							<button type="button" className="menu-row model-picker-back" onClick={back}>
								<ChevronLeft size={14} aria-hidden="true" />
								<span>返回</span>
							</button>
							<ListBox
								ref={listBox}
								key={listPage}
								aria-label={listPage === "models" ? "模型" : "思考强度"}
								autoFocus={page !== "main"}
								selectionMode="single"
								selectedKeys={selectedKey ? [selectedKey] : []}
								disabledKeys={busy ? options.map((option) => option.id) : []}
								onSelectionChange={(keys) => {
									if (keys === "all") return;
									const key = [...keys][0];
									if (key == null) back();
									else void choose(String(key));
								}}
							>
								{options.map((option) => (
									<ListBox.Item key={option.id} id={option.id} textValue={option.title}>
										<span className="model-picker-option-label" title={option.title}>
											{option.label}
										</span>
										<span className="model-picker-option-meta">
											{option.provider && (
												<span className="model-picker-option-provider" title={option.provider}>
													{option.provider}
												</span>
											)}
											{selectedKey === option.id && (
												<Check size={14} className="model-picker-option-check" aria-hidden="true" />
											)}
										</span>
									</ListBox.Item>
								))}
							</ListBox>
						</div>
					</div>
				</Popover.Dialog>
			</Popover.Content>
		</Popover>
	);
}
