import type { ServerResponse } from "node:http";
import { BitLockerExpiredError, BitLockerInputError } from "../../../diagnostics/bitlocker-web.ts";
import { ModelCredentialsRequired } from "../../../runtime/model-selection.ts";
import { SessionOwnerConflict } from "../../../runtime/owner.ts";
import { ExportError } from "../exports/snapshot.ts";
import { InboxConflict } from "../session/inbox.ts";
import { problem } from "./http.ts";
import { RunbookOpenError } from "./runbook.ts";

export function sendError(response: ServerResponse, error: unknown): void {
	const diskCode =
		error && typeof error === "object" && "code" in error && typeof error.code === "string" ? error.code : undefined;
	if (error instanceof ModelCredentialsRequired) problem(response, 409, error.code, error.message);
	else if (error instanceof ExportError) problem(response, error.status, error.code, error.message);
	else if (error instanceof RunbookOpenError) problem(response, error.status, error.code, error.message);
	else if (error instanceof BitLockerInputError) problem(response, 400, "invalid_bitlocker_input", error.message);
	else if (error instanceof BitLockerExpiredError) problem(response, 409, "expired_request", error.message);
	else if (diskCode === "ENOSPC") problem(response, 507, "storage_full", "工具包存储空间不足，输入未被接受");
	else if (["EACCES", "EPERM", "EROFS", "ENOTDIR", "EEXIST"].includes(diskCode ?? ""))
		problem(response, 500, "storage_unavailable", "工具包目录无法写入，输入未被接受");
	else if (error instanceof Error && error.message === "Already compacted")
		problem(response, 409, "already_compacted", "会话已压缩，继续对话后可再次压缩");
	else if (error instanceof Error && error.message === "Nothing to compact (session too small)")
		problem(response, 409, "nothing_to_compact", "当前没有可压缩的较早内容");
	else if (diskCode === "CST_CONFIGURATION_UNAVAILABLE" && error instanceof Error)
		problem(response, 503, "configuration_unavailable", error.message);
	else if (error instanceof SessionOwnerConflict || diskCode === "CST_SESSION_OWNED")
		problem(
			response,
			409,
			"session_owned",
			error instanceof Error ? error.message : "该会话正由另一端使用，请先关闭持有该会话的程序。",
		);
	else if (error instanceof InboxConflict) problem(response, 409, "conflict", error.message);
	else if (error instanceof SyntaxError) problem(response, 400, "invalid_json", "请求须为有效 JSON");
	else if (error instanceof Error && /^(请求|消息|队列|缺少|操作|提交|图片|回答|选项)/.test(error.message)) {
		problem(response, 400, "invalid_request", error.message);
	} else if (error instanceof Error && /会话已删除/.test(error.message))
		problem(response, 409, "conflict", "创建请求对应的会话已删除");
	else if (error instanceof Error && /会话不存在/.test(error.message))
		problem(response, 404, "not_found", "会话不存在");
	else problem(response, 500, "server_error", "操作未完成，请稍后重试");
}
