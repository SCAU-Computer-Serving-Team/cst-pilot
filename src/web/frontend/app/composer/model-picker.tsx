import { Button, ListBox, Popover } from "@heroui/react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { type CSSProperties, useLayoutEffect, useRef, useState } from "react";
import type { ModelChoice } from "../data/model-selection";
import type { Model } from "../data/web-state";
import { usePanelDismiss } from "../shell/panel-dismiss";

const fullId = (model: Model) => `${model.provider}/${model.id}`;
function thinkingLabel(level = "off") {
	return level === "xhigh" ? "XHigh" : level.charAt(0).toUpperCase() + level.slice(1);
}
type Page = "main" | "models" | "thinking";

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
	const returnFocus = useRef<Exclude<Page, "main"> | null>(null);
	const modelButton = useRef<HTMLButtonElement>(null);
	const thinkingButton = useRef<HTMLButtonElement>(null);
	const listBox = useRef<HTMLDivElement>(null);
	usePanelDismiss(open, () => {
		navigation.current++;
		setOpen(false);
	});
	useLayoutEffect(() => {
		if (!open || page === "main") return;
		function returnToMain(event: KeyboardEvent) {
			if (event.key !== "Escape" || event.isComposing || event.defaultPrevented) return;
			event.preventDefault();
			event.stopPropagation();
			returnFocus.current = page === "models" ? "models" : "thinking";
			navigation.current++;
			setPage("main");
		}
		window.addEventListener("keydown", returnToMain, true);
		return () => window.removeEventListener("keydown", returnToMain, true);
	}, [open, page]);
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
	const supportedLevels = levels.length ? levels : ["off"];
	const options =
		listPage === "models"
			? models.map((model) => ({ id: fullId(model), label: model.name, title: `${model.provider} · ${model.name}` }))
			: supportedLevels.map((level) => ({ id: level, label: thinkingLabel(level), title: thinkingLabel(level) }));
	const selectedKey = listPage === "models" ? modelKey : thinking;
	const rows = page === "main" ? 2 : Math.max(1, options.length);
	return (
		<Popover
			isOpen={open}
			onOpenChange={(next) => {
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
				variant="ghost"
				className="composer-model"
				aria-label={`模型与思考强度：${selected?.name ?? "加载中"}，${thinkingLabel(thinking)}`}
				isDisabled={!selected && !models.length}
			>
				<span className="model-name" title={selected?.name}>
					{selected?.name ?? "选择模型"}
				</span>
				<span className="model-current-strength">{thinkingLabel(thinking)}</span>
				<span className="model-chevron">
					<ChevronDown size={14} aria-hidden="true" />
				</span>
			</Button>
			<Popover.Content
				className={`composer-popover model-picker-popover t-resize ${page === "main" ? "" : "model-picker-list"}`}
				style={{ "--picker-height": `${rows * 32 + 24}px` } as CSSProperties}
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
								className="model-picker-model"
								aria-label="选择模型"
								isDisabled={!models.length}
								onPress={() => {
									navigation.current++;
									setListPage("models");
									setPage("models");
								}}
							>
								<span title={selected?.name}>{selected?.name ?? "选择模型"}</span>
								<ChevronRight size={16} aria-hidden="true" />
							</Button>
							<Button
								ref={thinkingButton}
								variant="ghost"
								className="model-picker-thinking"
								aria-label="选择思考强度"
								onPress={() => {
									navigation.current++;
									setListPage("thinking");
									setPage("thinking");
								}}
							>
								<span>{thinkingLabel(thinking)}</span>
								<ChevronRight size={16} aria-hidden="true" />
							</Button>
						</div>
						<div
							className="model-picker-options t-page"
							data-page-id="2"
							aria-hidden={page === "main"}
							inert={page === "main"}
						>
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
										<span title={option.title}>{option.label}</span>
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
