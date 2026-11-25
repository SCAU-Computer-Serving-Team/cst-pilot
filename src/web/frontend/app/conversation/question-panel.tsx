import { Button, Input, Label, ListBox, Select, TextArea, TextField } from "@heroui/react";
import { ChevronDown } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { apiJson, sessionPath } from "../data/api";
import type { Question } from "../data/web-state";
import { handleTextFieldVerticalArrow } from "../shell/input-shortcuts";
import { useWorkspaceShell } from "../shell/workspace-shell-context";

export function QuestionPanel({
	id,
	question,
	refresh,
}: {
	id: string;
	question: Question;
	refresh: () => Promise<void>;
}) {
	const [value, setValue] = useState(question.prefill ?? "");
	const [selectOpen, setSelectOpen] = useState(false);
	const [busy, setBusy] = useState(false);
	const busyRef = useRef(false);
	const [error, setError] = useState("");
	const shell = useWorkspaceShell();
	useEffect(() => {
		if (shell?.appActive) setSelectOpen(false);
	}, [shell?.appActive]);
	async function respond(answer: string | boolean | null) {
		if (busyRef.current) return;
		busyRef.current = true;
		setBusy(true);
		try {
			await apiJson(sessionPath(id, `/ui/${encodeURIComponent(question.requestId)}/response`), {
				method: "POST",
				body: { value: answer },
			});
			setError("");
			await refresh();
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "回答未送达");
			await refresh();
		} finally {
			busyRef.current = false;
			setBusy(false);
		}
	}
	return (
		<section className="question-panel" aria-label="扩展提问" key={question.requestId}>
			<div className="question-head">
				<strong>{question.title}</strong>
				{question.message && <p>{question.message}</p>}
			</div>
			{question.kind === "confirm" ? (
				<div className="question-controls">
					<Button
						className="tier-1-btn-primary question-apply"
						isDisabled={busy}
						onPress={() => void respond(true)}
					>
						确认
					</Button>
					<Button
						variant="ghost"
						className="btn-ghost question-skip"
						isDisabled={busy}
						onPress={() => void respond(false)}
					>
						取消
					</Button>
				</div>
			) : (
				<div className="question-controls">
					{question.kind === "select" ? (
						<Select
							aria-label="选择答案"
							className="question-select"
							isDisabled={busy}
							isOpen={selectOpen}
							onOpenChange={setSelectOpen}
							selectedKey={value || null}
							onSelectionChange={(key) => setValue(key === null ? "" : String(key))}
						>
							<Label className="visually-hidden">选择答案</Label>
							<Select.Trigger className="question-select-trigger">
								<span>{value || "请选择"}</span>
								<ChevronDown size={14} aria-hidden="true" />
							</Select.Trigger>
							<Select.Popover className="tier-2-glass question-select-popover">
								<ListBox className="tier-2-menu-list">
									{question.options?.map((option) => (
										<ListBox.Item key={option} id={option} textValue={option} className="menu-row">
											{option}
										</ListBox.Item>
									))}
								</ListBox>
							</Select.Popover>
						</Select>
					) : question.kind === "editor" ? (
						<TextField
							aria-label="输入回答"
							className="question-field"
							isDisabled={busy}
							value={value}
							onChange={setValue}
						>
							<Label className="visually-hidden">输入回答</Label>
							<TextArea
								className="question-textarea"
								placeholder="输入回答"
								onKeyDown={handleTextFieldVerticalArrow}
							/>
						</TextField>
					) : (
						<TextField
							aria-label="输入回答"
							className="question-field"
							isDisabled={busy}
							value={value}
							onChange={setValue}
						>
							<Label className="visually-hidden">输入回答</Label>
							<Input
								className="question-input"
								placeholder="输入回答"
								onKeyDown={handleTextFieldVerticalArrow}
							/>
						</TextField>
					)}
					<div className="question-action-buttons">
						<Button
							className="tier-1-btn-primary question-apply"
							isDisabled={busy || (question.kind === "select" && !value)}
							onPress={() => void respond(value)}
						>
							提交
						</Button>
						<Button
							variant="ghost"
							className="btn-ghost question-skip"
							isDisabled={busy}
							onPress={() => void respond(null)}
						>
							跳过
						</Button>
					</div>
				</div>
			)}
			{error && (
				<p role="alert" className="question-error">
					{error}
				</p>
			)}
		</section>
	);
}
