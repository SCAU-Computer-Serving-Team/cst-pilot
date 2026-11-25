import assert from "node:assert/strict";
import { test } from "node:test";
import {
	buildCommandEntries,
	buildQuickOpenEntries,
	extractActiveSessionId,
	filterPaletteEntries,
	findFirstEnabledIndex,
	findLastEnabledIndex,
	formatExportBytes,
	formatPaletteTime,
	resolveActiveIndex,
	stepEnabledIndex,
	willPaletteTargetChangeRoute,
} from "./palette-model.ts";

test("extractActiveSessionId resolves chat, tree, and export session paths", () => {
	assert.equal(extractActiveSessionId("/s/abc-123"), "abc-123");
	assert.equal(extractActiveSessionId("/s/abc-123/tree"), "abc-123");
	assert.equal(extractActiveSessionId("/s/abc-123/export"), "abc-123");
	assert.equal(extractActiveSessionId("/s/session%201"), "session 1");
	assert.equal(extractActiveSessionId("/"), undefined);
	assert.equal(extractActiveSessionId("/sessions"), undefined);
	assert.equal(extractActiveSessionId("/settings/shortcuts"), undefined);
});

test("buildCommandEntries disables slash commands outside an active session and omits desktop-only OpenWeb", () => {
	const inactive = buildCommandEntries(false);
	assert.equal(inactive.length, 8);
	assert.deepEqual(
		inactive.slice(0, 4).map((e) => [e.title, e.disabled]),
		[
			["export", true],
			["tree", true],
			["compact", true],
			["fork", true],
		],
	);
	assert.deepEqual(
		inactive.slice(4).map((e) => [e.title, e.trailingKbd ?? null, e.disabled]),
		[
			["新建对话", "Alt+N", false],
			["打开全部会话", null, false],
			["切换侧边栏显示", "Ctrl+B", false],
			["打开设置", "Ctrl+,", false],
		],
	);
	const active = buildCommandEntries(true);
	assert.equal(
		active.every((e) => !e.disabled),
		true,
	);
});

test("buildQuickOpenEntries sorts sessions and completed exports with current marker and format metadata", () => {
	const now = new Date(2026, 10, 24, 16, 0, 0);
	const todayIso = new Date(2026, 10, 24, 14, 32, 0).toISOString();
	const olderIso = new Date(2026, 8, 14, 10, 0, 0).toISOString();
	const entries = buildQuickOpenEntries(
		[
			{ id: "s-old", title: "旧对话", updatedAt: olderIso },
			{ id: "s-now", title: "C 盘空间与硬盘信息", updatedAt: todayIso },
			{ id: "s-blank", title: "   ", updatedAt: "2026-09-10T08:00:00.000Z" },
		],
		"s-now",
		[
			{
				id: "e-pending",
				sessionId: "s-now",
				title: "进行中",
				capturedAt: todayIso,
				options: { format: "png" },
				status: "running",
				files: [],
			},
			{
				id: "e-html",
				sessionId: "s-old",
				title: "",
				capturedAt: olderIso,
				options: { format: "html" },
				status: "completed",
				files: [{ name: "report.html", bytes: 49152, url: "/api/exports/e-html/files/0" }],
			},
			{
				id: "e-png",
				sessionId: "s-now",
				title: "C 盘诊断",
				capturedAt: todayIso,
				options: { format: "png" },
				status: "completed",
				files: [
					{ name: "p1.png", bytes: 1000, url: "/api/exports/e-png/files/0" },
					{ name: "p2.png", bytes: 1200, url: "/api/exports/e-png/files/1" },
				],
			},
		],
		now,
	);

	assert.equal(entries.length, 5);
	assert.equal(entries[0]?.title, "C 盘空间与硬盘信息");
	assert.equal(entries[0]?.subtitle, "当前会话");
	assert.equal(entries[0]?.trailingMeta, "14:32");
	assert.equal(entries[1]?.trailingMeta, "2026/09/14");
	assert.equal(entries[2]?.title, "新对话");
	assert.equal(entries[3]?.title, "C 盘诊断");
	assert.equal(entries[3]?.subtitle, "PNG · 2 页");
	assert.equal(entries[4]?.title, "report.html");
	assert.equal(entries[4]?.subtitle, "HTML · 48 KB");
});

test("formatExportBytes and formatPaletteTime handle edge values", () => {
	assert.equal(formatExportBytes(0), "0 B");
	assert.equal(formatExportBytes(512), "512 B");
	assert.equal(formatExportBytes(49152), "48 KB");
	assert.equal(formatExportBytes(2 * 1024 * 1024 + 512 * 1024), "2.5 MB");
	assert.equal(formatPaletteTime(""), undefined);
	assert.equal(formatPaletteTime("2026-09-14"), "2026/09/14");
	assert.equal(formatPaletteTime("2026-01-01"), "2026/01/01");
});

test("filterPaletteEntries and stepEnabledIndex skip disabled items and wrap around", () => {
	const entries = buildCommandEntries(false);
	assert.equal(findFirstEnabledIndex(entries), 4);
	assert.equal(findLastEnabledIndex(entries), 7);
	assert.equal(stepEnabledIndex(entries, 7, 1), 4);
	assert.equal(stepEnabledIndex(entries, 4, -1), 7);
	assert.equal(stepEnabledIndex(entries, 4, 1), 5);

	const filtered = filterPaletteEntries(entries, "侧边栏 ctrl+b");
	assert.equal(filtered.length, 1);
	assert.equal(filtered[0]?.target.kind, "toggle_sidebar");

	const disabledOnly = filterPaletteEntries(entries, "export");
	assert.equal(disabledOnly.length, 1);
	assert.equal(findFirstEnabledIndex(disabledOnly), -1);
	assert.equal(stepEnabledIndex(disabledOnly, 0, 1), -1);
});

test("resolveActiveIndex keeps selection by ID across background reorders and falls back when removed", () => {
	const initial = buildQuickOpenEntries(
		[
			{ id: "s1", title: "会话一", updatedAt: "2026-11-24T10:00:00.000Z" },
			{ id: "s2", title: "会话二", updatedAt: "2026-11-24T09:00:00.000Z" },
		],
		undefined,
		[],
	);
	assert.equal(resolveActiveIndex(initial, "sess-s2"), 1);

	const reordered = buildQuickOpenEntries(
		[
			{ id: "s1", title: "会话一", updatedAt: "2026-11-24T10:00:00.000Z" },
			{ id: "s2", title: "会话二", updatedAt: "2026-11-24T11:00:00.000Z" },
		],
		undefined,
		[],
	);
	assert.equal(resolveActiveIndex(reordered, "sess-s2"), 0);
	assert.equal(resolveActiveIndex(reordered, "sess-missing"), 0);
	assert.equal(resolveActiveIndex([], "sess-s2"), -1);
});

test("willPaletteTargetChangeRoute distinguishes same-page actions from route changes", () => {
	assert.equal(willPaletteTargetChangeRoute({ kind: "all_sessions" }, "/sessions", undefined), false);
	assert.equal(willPaletteTargetChangeRoute({ kind: "all_sessions" }, "/", undefined), true);
	assert.equal(willPaletteTargetChangeRoute({ kind: "open_settings" }, "/settings", undefined), false);
	assert.equal(willPaletteTargetChangeRoute({ kind: "open_session", sessionId: "abc" }, "/s/abc", "abc"), false);
	assert.equal(willPaletteTargetChangeRoute({ kind: "open_session", sessionId: "def" }, "/s/abc", "abc"), true);
	assert.equal(willPaletteTargetChangeRoute({ kind: "toggle_sidebar" }, "/sessions", undefined), false);
	assert.equal(willPaletteTargetChangeRoute({ kind: "slash_fork" }, "/s/abc", "abc"), false);
});
