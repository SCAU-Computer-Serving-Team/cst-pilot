import { Button, Input, Label, ListBox, Select, TextArea, TextField } from "@heroui/react";
import { ChevronDown } from "lucide-react";
import { useState } from "react";
import { apiJson, sessionPath } from "../data/api";
import type { Question } from "../data/web-state";

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
	const [error, setError] = useState("");
	async function respond(answer: string | boolean | null) {
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
		}
	}
	return (
		<section className="question-panel" aria-label="扩展提问" key={question.requestId}>
			<strong>{question.title}</strong>
			{question.message && <p>{question.message}</p>}
			{question.kind === "confirm" ? (
				<div>
					<Button className="question-apply" onPress={() => void respond(true)}>
						确认
					</Button>
					<Button variant="ghost" className="question-skip" onPress={() => void respond(false)}>
						取消
					</Button>
				</div>
			) : (
				<>
					{question.kind === "select" ? (
						<Select
							aria-label="选择答案"
							className="question-select"
							selectedKey={value || null}
							onSelectionChange={(key) => setValue(key === null ? "" : String(key))}
						>
							<Label className="visually-hidden">选择答案</Label>
							<Select.Trigger className="question-select-trigger">
								<span>{value || "请选择"}</span>
								<ChevronDown size={14} aria-hidden="true" />
							</Select.Trigger>
							<Select.Popover>
								<ListBox>
									{question.options?.map((option) => (
										<ListBox.Item key={option} id={option} textValue={option}>
											{option}
										</ListBox.Item>
									))}
								</ListBox>
							</Select.Popover>
						</Select>
					) : question.kind === "editor" ? (
						<TextField aria-label="输入回答" className="question-field" value={value} onChange={setValue}>
							<Label className="visually-hidden">输入回答</Label>
							<TextArea className="question-textarea" placeholder="输入回答" />
						</TextField>
					) : (
						<TextField aria-label="输入回答" className="question-field" value={value} onChange={setValue}>
							<Label className="visually-hidden">输入回答</Label>
							<Input className="question-input" placeholder="输入回答" />
						</TextField>
					)}
					<Button
						className="question-apply"
						isDisabled={question.kind === "select" && !value}
						onPress={() => void respond(value)}
					>
						提交
					</Button>
					<Button variant="ghost" className="question-skip" onPress={() => void respond(null)}>
						跳过
					</Button>
				</>
			)}
			{error && <p role="alert">{error}</p>}
		</section>
	);
}
