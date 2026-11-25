import { Button, Popover } from "@heroui/react";
import {
	ChevronDown,
	CircleUserRound,
	History,
	LayoutDashboard,
	LogIn,
	MessageSquareWarning,
	PanelLeft,
	Power,
	Search,
	Settings,
	SquarePen,
	SunMoon,
	User,
	X,
} from "lucide-react";
import {
	type CSSProperties,
	Fragment,
	useCallback,
	useEffect,
	useLayoutEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import { Link, PrefetchPageLinks, useLocation, useNavigate } from "react-router";
import { describeAccount } from "../account/format";
import { useAccount } from "../account/use-account";
import { accountProvider, loginPath } from "../auth/providers";
import { Composer, type ComposerConfig } from "../composer/composer";
import { UserMessage } from "../conversation/user-message";
import { apiJson, type ProviderStatus, sessionPath } from "../data/api";
import { attachTelemetryHints } from "../data/telemetry-hints";
import { outgoingFor, useMessageOutbox } from "../data/use-message-outbox";
import {
	type ContentSearchMatch,
	getCachedSessionDetails,
	invalidateCachedSessionDetail,
	type SearchMatchSpan,
	searchSessionsApi,
	useGlobalEvents,
} from "../data/web-state";
import { extractActiveSessionId } from "../palette/palette-model";
import { BlueHour } from "./blue-hour";
import { ExitDialog } from "./exit-dialog";
import { handleTextFieldVerticalArrow } from "./input-shortcuts";
import { usePanelDismiss } from "./panel-dismiss";
import { sampleSessions } from "./sample-sessions";
import { matchQuery } from "./search-matcher";
import { SessionActionsOverlay, useSessionActions } from "./session-actions";
import { SessionEntry } from "./session-entry";
import { filterRecentSessions, RECENT_SESSION_LIMIT } from "./session-list";
import { SessionManagementToolbar } from "./session-management";
import { useWorkspaceSessions } from "./sessions-context";
import {
	applyFirstSearchPage,
	beginSidebarSearchQuery,
	clearSidebarSearchState,
	clickExpandContents,
	clickExpandTitles,
	createInitialSidebarSearchState,
	finishContentPrefetch,
	finishTitlePrefetch,
	SEARCH_PAGE_SIZE,
	type SidebarSearchState,
	searchLocalPreloadedSessions,
} from "./sidebar-search";
import { currentTheme, nextTheme, type Theme, themeLabels } from "./theme";

function avatarInitialFor(label: string): string {
	const trimmed = label.trim();
	if (!trimmed) return "";
	return Array.from(trimmed)[0]?.toUpperCase() ?? "";
}

function HighlightedText({ text, spans }: { text: string; spans: readonly SearchMatchSpan[] }) {
	if (!spans.length) return <>{text}</>;
	const nodes: React.ReactNode[] = [];
	let cursor = 0;
	for (const span of spans) {
		const start = Math.max(cursor, Math.min(span.start, text.length));
		const end = Math.max(start, Math.min(span.end, text.length));
		if (start > cursor) nodes.push(<Fragment key={`p-${cursor}`}>{text.slice(cursor, start)}</Fragment>);
		if (end > start) {
			nodes.push(
				<mark key={`m-${start}`} className="sidebar-search-mark">
					{text.slice(start, end)}
				</mark>,
			);
		}
		cursor = end;
	}
	if (cursor < text.length) nodes.push(<Fragment key={`t-${cursor}`}>{text.slice(cursor)}</Fragment>);
	return <>{nodes}</>;
}

function formatSearchShortDate(updatedAt: string | null | undefined): string | null {
	if (!updatedAt) return null;
	const dt = new Date(updatedAt);
	if (Number.isNaN(dt.getTime())) return null;
	const now = new Date();
	const sameDay =
		dt.getFullYear() === now.getFullYear() && dt.getMonth() === now.getMonth() && dt.getDate() === now.getDate();
	if (sameDay) {
		return `${String(dt.getHours()).padStart(2, "0")}:${String(dt.getMinutes()).padStart(2, "0")}`;
	}
	return `${String(dt.getMonth() + 1).padStart(2, "0")}/${String(dt.getDate()).padStart(2, "0")}`;
}

export function Sidebar({
	preview = false,
	startup = false,
	collapsed = false,
	sidebarRef,
	collapseButtonRef,
	onCollapse,
	onNavigate,
}: {
	preview?: boolean;
	startup?: boolean;
	collapsed?: boolean;
	sidebarRef?: React.RefObject<HTMLElement | null>;
	collapseButtonRef?: React.RefObject<HTMLButtonElement | null>;
	onCollapse: () => void;
	onNavigate?: () => void;
}) {
	useEffect(() => {
		if (!preview) return attachTelemetryHints(window);
	}, [preview]);
	const location = useLocation();
	const navigate = useNavigate();
	const state = useWorkspaceSessions();
	const ready = preview || !!state.stage || !!state.error;
	const startupPendingList = startup && !ready;
	const [recentOpen, setRecentOpen] = useState(true);
	const [searchQuery, setSearchQuery] = useState("");
	const searchInputRef = useRef<HTMLInputElement>(null);
	const [searchState, setSearchState] = useState<SidebarSearchState>(createInitialSidebarSearchState);
	const searchStateRef = useRef(searchState);
	searchStateRef.current = searchState;
	const searchDebounceRef = useRef<number | undefined>(undefined);
	const searchAbortRef = useRef<AbortController | null>(null);
	const prefetchTitleAbortRef = useRef<AbortController | null>(null);
	const prefetchContentAbortRef = useRef<AbortController | null>(null);
	const startupPending = useRef(startup);
	const [startupRows, setStartupRows] = useState(false);
	useLayoutEffect(() => {
		if (!startup) {
			startupPending.current = false;
			setStartupRows(false);
			return;
		}
		if (!startupPending.current || !ready) return;
		startupPending.current = false;
		setStartupRows(true);
		const timer = window.setTimeout(() => setStartupRows(false), 1100);
		return () => window.clearTimeout(timer);
	}, [startup, ready]);
	const sourceSessions = preview ? sampleSessions : state.sessions;
	const activeSessionId = useMemo(() => extractActiveSessionId(location.pathname), [location.pathname]);
	const sessions = useMemo(() => filterRecentSessions(sourceSessions, "", RECENT_SESSION_LIMIT), [sourceSessions]);
	const hasSearchQuery = searchQuery.trim().length > 0;

	const cancelSearchRequests = useCallback(() => {
		window.clearTimeout(searchDebounceRef.current);
		searchDebounceRef.current = undefined;
		searchAbortRef.current?.abort();
		searchAbortRef.current = null;
		prefetchTitleAbortRef.current?.abort();
		prefetchTitleAbortRef.current = null;
		prefetchContentAbortRef.current?.abort();
		prefetchContentAbortRef.current = null;
	}, []);

	useEffect(() => () => cancelSearchRequests(), [cancelSearchRequests]);

	const triggerTitlePrefetch = useCallback(
		(seq: number, query: string, offset: number, snapshotToken: string) => {
			if (preview || !query.trim()) return;
			prefetchTitleAbortRef.current?.abort();
			const controller = new AbortController();
			prefetchTitleAbortRef.current = controller;
			void searchSessionsApi(
				{
					query,
					activeSessionId,
					titleOffset: offset,
					titleLimit: SEARCH_PAGE_SIZE,
					contentOffset: 0,
					contentLimit: 1,
					snapshotToken: snapshotToken || undefined,
				},
				controller.signal,
			)
				.then((resp) => {
					if (controller.signal.aborted) return;
					const finished = finishTitlePrefetch(searchStateRef.current, seq, resp.titleMatches, resp.titleHasMore);
					searchStateRef.current = finished.state;
					setSearchState(finished.state);
					if (finished.nextPrefetchOffset !== undefined) {
						triggerTitlePrefetch(seq, query, finished.nextPrefetchOffset, finished.state.snapshotToken);
					}
				})
				.catch(() => {
					if (controller.signal.aborted) return;
					setSearchState((cur) =>
						cur.requestSeq === seq
							? { ...cur, prefetchTitleInFlight: false, autoExpandTitlePending: false }
							: cur,
					);
				});
		},
		[activeSessionId, preview],
	);

	const triggerContentPrefetch = useCallback(
		(seq: number, query: string, offset: number, snapshotToken: string) => {
			if (preview || !query.trim()) return;
			prefetchContentAbortRef.current?.abort();
			const controller = new AbortController();
			prefetchContentAbortRef.current = controller;
			void searchSessionsApi(
				{
					query,
					activeSessionId,
					titleOffset: 0,
					titleLimit: 1,
					contentOffset: offset,
					contentLimit: SEARCH_PAGE_SIZE,
					snapshotToken: snapshotToken || undefined,
				},
				controller.signal,
			)
				.then((resp) => {
					if (controller.signal.aborted) return;
					const finished = finishContentPrefetch(
						searchStateRef.current,
						seq,
						resp.contentMatches,
						resp.contentHasMore,
					);
					searchStateRef.current = finished.state;
					setSearchState(finished.state);
					if (finished.nextPrefetchOffset !== undefined) {
						triggerContentPrefetch(seq, query, finished.nextPrefetchOffset, finished.state.snapshotToken);
					}
				})
				.catch(() => {
					if (controller.signal.aborted) return;
					setSearchState((cur) =>
						cur.requestSeq === seq
							? { ...cur, prefetchContentInFlight: false, autoExpandContentPending: false }
							: cur,
					);
				});
		},
		[activeSessionId, preview],
	);

	const runSidebarSearch = useCallback(
		(rawQuery: string) => {
			cancelSearchRequests();
			const trimmed = rawQuery.trim();
			if (!trimmed) {
				const cleared = clearSidebarSearchState(searchStateRef.current);
				searchStateRef.current = cleared;
				setSearchState(cleared);
				return;
			}
			const localPreview = searchLocalPreloadedSessions(
				trimmed,
				activeSessionId,
				sourceSessions,
				getCachedSessionDetails(),
			);
			const next = beginSidebarSearchQuery(searchStateRef.current, trimmed, localPreview);
			const finalLocal = preview ? { ...next, loadingFirstPage: false, backendSynced: true } : next;
			searchStateRef.current = finalLocal;
			setSearchState(finalLocal);
			if (preview) return;

			const seq = finalLocal.requestSeq;
			searchDebounceRef.current = window.setTimeout(() => {
				const controller = new AbortController();
				searchAbortRef.current = controller;
				void searchSessionsApi(
					{
						query: trimmed,
						activeSessionId,
						titleOffset: 0,
						titleLimit: SEARCH_PAGE_SIZE,
						contentOffset: 0,
						contentLimit: SEARCH_PAGE_SIZE,
					},
					controller.signal,
				)
					.then((resp) => {
						if (controller.signal.aborted) return;
						const applied = applyFirstSearchPage(searchStateRef.current, seq, resp);
						searchStateRef.current = applied.state;
						setSearchState(applied.state);
						if (applied.prefetchTitleOffset !== undefined) {
							triggerTitlePrefetch(seq, trimmed, applied.prefetchTitleOffset, applied.state.snapshotToken);
						}
						if (applied.prefetchContentOffset !== undefined) {
							triggerContentPrefetch(seq, trimmed, applied.prefetchContentOffset, applied.state.snapshotToken);
						}
					})
					.catch(() => {
						if (controller.signal.aborted) return;
						setSearchState((cur) => (cur.requestSeq === seq ? { ...cur, loadingFirstPage: false } : cur));
					});
			}, 100);
		},
		[activeSessionId, cancelSearchRequests, preview, sourceSessions, triggerContentPrefetch, triggerTitlePrefetch],
	);

	// 会话列表更新时，若处于搜索态则刷新结果。
	useEffect(() => {
		if (!searchQuery.trim()) return;
		runSidebarSearch(searchQuery);
	}, [runSidebarSearch, searchQuery]);

	const clearSearch = useCallback(
		(focusInput = true) => {
			setSearchQuery("");
			runSidebarSearch("");
			if (focusInput) searchInputRef.current?.focus();
		},
		[runSidebarSearch],
	);

	const handleExpandTitles = useCallback(() => {
		const res = clickExpandTitles(searchStateRef.current);
		searchStateRef.current = res.state;
		setSearchState(res.state);
		if (res.action.kind === "appended" && res.action.nextPrefetchOffset !== undefined) {
			triggerTitlePrefetch(
				res.state.requestSeq,
				res.state.query,
				res.action.nextPrefetchOffset,
				res.state.snapshotToken,
			);
		} else if (res.action.kind === "fetch_now") {
			triggerTitlePrefetch(res.state.requestSeq, res.state.query, res.action.offset, res.state.snapshotToken);
		}
	}, [triggerTitlePrefetch]);

	const handleExpandContents = useCallback(() => {
		const res = clickExpandContents(searchStateRef.current);
		searchStateRef.current = res.state;
		setSearchState(res.state);
		if (res.action.kind === "appended" && res.action.nextPrefetchOffset !== undefined) {
			triggerContentPrefetch(
				res.state.requestSeq,
				res.state.query,
				res.action.nextPrefetchOffset,
				res.state.snapshotToken,
			);
		} else if (res.action.kind === "fetch_now") {
			triggerContentPrefetch(res.state.requestSeq, res.state.query, res.action.offset, res.state.snapshotToken);
		}
	}, [triggerContentPrefetch]);

	const handleSelectContentHit = useCallback(
		async (item: ContentSearchMatch) => {
			const sid = item.sessionId;
			const entryId = item.bestHit.entryId;
			const query = searchStateRef.current.query;
			const suffix = preview ? "?preview=1" : "";
			if (item.bestHit.onActiveBranch || preview) {
				navigate(`/s/${encodeURIComponent(sid)}${suffix}`, {
					state: { locateEntryId: entryId, locateQuery: query, locateNonce: Date.now() },
				});
				onNavigate?.();
				return;
			}
			const isRunning = sourceSessions.some((s) => s.id === sid && s.running);
			if (isRunning) {
				navigate(`/s/${encodeURIComponent(sid)}/tree?focus=${encodeURIComponent(entryId)}`);
				onNavigate?.();
				return;
			}
			try {
				let previousLeafId: string | null = null;
				try {
					const treeRes = await apiJson<{ leafId: string | null }>(sessionPath(sid, "/tree"));
					previousLeafId = treeRes.leafId;
				} catch {
					const cached = getCachedSessionDetails().get(sid);
					previousLeafId = cached?.entries.at(-1)?.id ?? null;
				}
				const targetLeaf = item.bestHit.branchLeafId ?? entryId;
				await apiJson(sessionPath(sid, "/tree/navigate"), {
					method: "POST",
					body: { entryId: targetLeaf, summarize: false },
				});
				invalidateCachedSessionDetail(sid);
				navigate(`/s/${encodeURIComponent(sid)}`, {
					state: {
						locateEntryId: entryId,
						locateQuery: query,
						locateNonce: Date.now(),
						historyBranch: previousLeafId ? { previousLeafId } : undefined,
					},
				});
				onNavigate?.();
			} catch {
				navigate(`/s/${encodeURIComponent(sid)}/tree?focus=${encodeURIComponent(entryId)}`);
				onNavigate?.();
			}
		},
		[navigate, onNavigate, preview, sourceSessions],
	);
	const [providers, setProviders] = useState<ProviderStatus[]>([]);
	const team = accountProvider(providers);
	const teamSignedIn = team?.type === "oauth" || team?.type === "api_key";
	const [accountError, setAccountError] = useState("");
	const [accountWorking, setAccountWorking] = useState(false);
	const [accountMenuOpen, setAccountMenuOpen] = useState(false);
	const [exitOpen, setExitOpen] = useState(false);
	// 账号菜单只显示身份；打开时按缓存时效同步。
	const { account: cstoaAccount, refresh: refreshCstoaAccount } = useAccount();
	const cstoaSummary = describeAccount(cstoaAccount);
	const teamRequiresLogin = team?.requiresLogin || cstoaAccount?.requiresLogin;
	const accountTriggerRef = useRef<HTMLButtonElement>(null);
	usePanelDismiss(accountMenuOpen, () => setAccountMenuOpen(false), accountTriggerRef);
	function closeAccountMenu() {
		setAccountMenuOpen(false);
		onNavigate?.();
	}
	const [theme, setTheme] = useState<Theme>(currentTheme);
	const topRef = useRef<HTMLDivElement>(null);
	const focusSession = useCallback((id?: string) => {
		requestAnimationFrame(() => {
			const row = id
				? topRef.current?.querySelector<HTMLElement>(`[data-session-id="${CSS.escape(id)}"]`)
				: topRef.current?.querySelector<HTMLElement>(".sidebar-new");
			(row?.querySelector<HTMLElement>("input") ?? row)?.focus();
		});
	}, []);
	const actions = useSessionActions(sessions, focusSession, preview);
	const { management, setMenu, clearRenaming, openSessionMenu } = actions;
	const finishManagement = management.finish;
	const retainSelection = management.retain;
	const appActive = !preview && state.stage === "app_active";
	const inactive = preview || appActive;

	// 侧栏折叠或切入软件端待机态时，关闭所有悬浮层/弹窗并退出多选，避免穿透遮罩。
	useEffect(() => {
		if (!collapsed && !appActive) return;
		setAccountMenuOpen(false);
		setMenu(null);
		clearRenaming();
		finishManagement();
		if (appActive) setExitOpen(false);
	}, [collapsed, appActive, finishManagement, setMenu, clearRenaming]);

	// 搜索过滤变化时，多选集合仅保留当前可见会话，防止隐藏项残留并在清空后复活。
	useEffect(() => {
		if (!management.managing) return;
		retainSelection(new Set(sessions.map((session) => session.id)));
	}, [management.managing, sessions, retainSelection]);

	const refreshAccount = useCallback(async () => {
		if (inactive) return;
		try {
			const data = await apiJson<{ providers: ProviderStatus[] }>("/api/auth");
			setProviders(data.providers);
			setAccountError("");
		} catch (cause) {
			setAccountError(cause instanceof Error ? cause.message : "无法读取登录状态");
		}
	}, [inactive]);
	const refreshTheme = useCallback(async () => {
		if (inactive) return;
		try {
			const settings = await apiJson<{ theme: Theme }>("/api/settings");
			document.documentElement.dataset.theme = settings.theme;
			setTheme(settings.theme);
		} catch {
			/* 页面仍使用当前主题；主题按钮提交失败时另行提示。 */
		}
	}, [inactive]);
	useEffect(() => {
		const root = document.documentElement;
		const observer = new MutationObserver(() => setTheme(currentTheme()));
		observer.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
		return () => observer.disconnect();
	}, []);
	useEffect(() => {
		if (inactive) return;
		void refreshAccount();
		void refreshTheme();
	}, [inactive, refreshAccount, refreshTheme]);
	useGlobalEvents((name, event) => {
		if (name === "reset") {
			void refreshAccount();
			void refreshTheme();
			return;
		}
		if (name !== "state") return;
		try {
			const type = JSON.parse(event?.data ?? "null")?.type;
			if (type === "auth_changed") void refreshAccount();
			if (type === "settings_changed") void refreshTheme();
		} catch {
			/* 其他状态事件不影响当前主题。 */
		}
	}, !inactive);
	async function changeTheme() {
		const value = nextTheme[theme];
		if (preview) {
			document.documentElement.dataset.theme = value;
			setTheme(value);
			return;
		}
		setAccountWorking(true);
		try {
			await apiJson("/api/settings", { method: "PATCH", body: { theme: value } });
			document.documentElement.dataset.theme = value;
			setTheme(value);
			setAccountError("");
		} catch (cause) {
			setAccountError(cause instanceof Error ? cause.message : "主题切换失败，请重试");
		} finally {
			setAccountWorking(false);
		}
	}

	const homeHref = preview ? "/?preview=1" : "/";
	const accountDisplayName = preview
		? "示例账号"
		: cstoaAccount?.signedIn && !teamRequiresLogin
			? cstoaAccount.profile.name || cstoaAccount.profile.studentId || "CSTOA 账号"
			: teamSignedIn
				? teamRequiresLogin
					? "需要重新登录"
					: "CSTOA 账号"
				: state.stage === "preview"
					? "页面预览"
					: "CSTOA 未登录";
	const accountInitial =
		preview || (cstoaAccount?.signedIn && !teamRequiresLogin) ? avatarInitialFor(accountDisplayName) : "";

	return (
		<aside
			ref={sidebarRef}
			className="sidebar"
			aria-label="会话导航"
			data-managing={management.managing || undefined}
			data-startup-rows={startupRows || undefined}
			data-startup-pending={startupPendingList || undefined}
			inert={collapsed}
			aria-hidden={collapsed}
		>
			{/* 所有会话共用聊天路由，只插入一份预载声明。 */}
			{sessions[0] && <PrefetchPageLinks page={`/s/${sessions[0].id}${preview ? "?preview=1" : ""}`} />}
			<div className="sidebar-top" ref={topRef}>
				{/* 1. 品牌与收起/新建操作行（b50Q7Y，高 40px） */}
				<div className="sidebar-brand">
					<Link className="product-name" to={homeHref} viewTransition onClick={onNavigate}>
						CST Pilot
					</Link>
					<div className="sidebar-brand-actions">
						<button
							ref={collapseButtonRef}
							type="button"
							className="btn-icon sidebar-icon"
							aria-label="收起侧栏"
							title="收起侧栏"
							onClick={onCollapse}
						>
							<PanelLeft size={18} aria-hidden="true" />
						</button>
						<Link
							className={`btn-icon sidebar-new ${!management.managing && location.pathname === "/" ? "is-home" : ""}`}
							to={homeHref}
							aria-label="新建对话"
							title="新建对话"
							viewTransition
							onClick={onNavigate}
						>
							<SquarePen size={18} aria-hidden="true" />
						</Link>
					</div>
				</div>

				{/* 2. 主体导航与最近列表容器（pQpX5，gap 16px） */}
				<div className="sidebar-body">
					{/* 2a. 搜索记录框（oq4G4，高 36px） */}
					<div className="sidebar-search-box" data-active={hasSearchQuery || undefined}>
						<Search size={18} className="sidebar-search-icon" aria-hidden="true" />
						<input
							ref={searchInputRef}
							type="search"
							className="sidebar-search-input"
							placeholder="搜索"
							aria-label="搜索会话"
							value={searchQuery}
							onChange={(event) => {
								const next = event.target.value;
								setSearchQuery(next);
								if (!next.trim()) runSidebarSearch("");
							}}
							onKeyDown={(event) => {
								if (event.nativeEvent.isComposing || event.keyCode === 229) return;
								if (event.key === "Escape" && searchQuery) {
									event.preventDefault();
									event.stopPropagation();
									clearSearch(true);
									return;
								}
								handleTextFieldVerticalArrow(event);
							}}
						/>
						{hasSearchQuery && (
							<button
								type="button"
								className="sidebar-search-clear"
								aria-label="清空搜索"
								onClick={() => clearSearch(true)}
							>
								<X size={13} aria-hidden="true" />
							</button>
						)}
					</div>

					{hasSearchQuery ? (
						<section className="sidebar-search-panel" aria-label="搜索结果">
							{searchState.titleMatches.length === 0 && searchState.contentMatches.length === 0 ? (
								searchState.loadingFirstPage ? (
									<div className="sidebar-search-empty">
										<p className="sidebar-search-empty-sub">正在搜索…</p>
									</div>
								) : (
									<div className="sidebar-search-empty">
										<div className="sidebar-search-empty-badge" aria-hidden="true">
											<Search size={16} className="sidebar-search-empty-icon" />
										</div>
										<div className="sidebar-search-empty-copy">
											<p className="sidebar-search-empty-title">无匹配结果</p>
											<p className="sidebar-search-empty-sub">清空后恢复导航与列表</p>
										</div>
										<button
											type="button"
											className="tier-1-btn-outline sidebar-search-empty-btn"
											onClick={() => clearSearch(true)}
										>
											<X size={14} aria-hidden="true" />
											<span>清空搜索</span>
										</button>
									</div>
								)
							) : (
								<div className="sidebar-search-results">
									{searchState.titleMatches.length > 0 && (
										<section className="sidebar-search-group" aria-label="标题匹配结果">
											<div className="sidebar-search-group-head">
												<span>标题</span>
												<span className="sidebar-search-group-count">{searchState.titleTotal}</span>
											</div>
											<div className="sidebar-search-group-list">
												{searchState.titleMatches.map((item) => {
													const active = activeSessionId === item.sessionId;
													const titleSpans = matchQuery(item.title, searchState.query)?.spans ?? [];
													const shortDate = formatSearchShortDate(item.updatedAt);
													return (
														<button
															key={item.sessionId}
															type="button"
															className="sidebar-search-title-row"
															data-active={active || undefined}
															onClick={() => {
																navigate(
																	`/s/${encodeURIComponent(item.sessionId)}${preview ? "?preview=1" : ""}`,
																);
																onNavigate?.();
															}}
														>
															<span className="sidebar-search-title-text">
																<HighlightedText text={item.title} spans={titleSpans} />
															</span>
															{shortDate && (
																<span className="sidebar-search-title-date">{shortDate}</span>
															)}
														</button>
													);
												})}
											</div>
											{searchState.titleHasMore && (
												<button
													type="button"
													className="sidebar-search-expand"
													onClick={handleExpandTitles}
												>
													<span>展开</span>
													<ChevronDown size={14} aria-hidden="true" />
												</button>
											)}
										</section>
									)}
									{searchState.titleMatches.length > 0 && searchState.contentMatches.length > 0 && (
										<hr className="sidebar-search-divider" />
									)}
									{searchState.contentMatches.length > 0 && (
										<section className="sidebar-search-group" aria-label="正文匹配结果">
											<div className="sidebar-search-group-head">
												<span>正文</span>
												<span className="sidebar-search-group-count">{searchState.contentTotal}</span>
											</div>
											<div className="sidebar-search-group-list">
												{searchState.contentMatches.map((item) => {
													const active = activeSessionId === item.sessionId;
													return (
														<button
															key={item.sessionId}
															type="button"
															className="sidebar-search-content-row"
															data-active={active || undefined}
															onClick={() => void handleSelectContentHit(item)}
														>
															<span className="sidebar-search-content-top">
																<span className="sidebar-search-content-title">{item.title}</span>
																<span className="sidebar-search-content-count">{item.hitCount} 处</span>
															</span>
															<span className="sidebar-search-content-snippet">
																<HighlightedText
																	text={item.bestHit.snippet}
																	spans={item.bestHit.spans}
																/>
															</span>
														</button>
													);
												})}
											</div>
											{searchState.contentHasMore && (
												<button
													type="button"
													className="sidebar-search-expand"
													onClick={handleExpandContents}
												>
													<span>展开</span>
													<ChevronDown size={14} aria-hidden="true" />
												</button>
											)}
										</section>
									)}
								</div>
							)}
						</section>
					) : (
						<>
							{/* 2b. 主功能导航（WKJAy，gap 6px） */}
							<nav className="sidebar-primary-nav" aria-label="主导航">
								<button
									type="button"
									className="sidebar-nav-item sidebar-dashboard"
									disabled
									title="仪表盘功能计划中"
								>
									<LayoutDashboard size={18} aria-hidden="true" />
									<span>仪表盘</span>
								</button>
								<Link
									className={`sidebar-nav-item sidebar-all ${!management.managing && location.pathname === "/sessions" ? "selected" : ""}`}
									to={preview ? "/sessions?preview=1" : "/sessions"}
									aria-current={location.pathname === "/sessions" ? "page" : undefined}
									viewTransition
									onClick={onNavigate}
								>
									<History size={18} aria-hidden="true" />
									<span>全部会话</span>
								</Link>
							</nav>

							{/* 2c. 最近会话组（CzUNm / STl1A） */}
							<div className="sidebar-recent">
								<div className="sidebar-recent-bar">
									<button
										type="button"
										className="sidebar-recent-heading"
										aria-expanded={recentOpen}
										style={{ "--startup-row": 0 } as CSSProperties}
										onClick={() => {
											setMenu(null);
											if (recentOpen) finishManagement();
											setRecentOpen((open) => !open);
										}}
									>
										<span>最近</span>
										<ChevronDown
											size={14}
											className="disc-chevron"
											data-expanded={recentOpen ? "true" : "false"}
											aria-hidden="true"
										/>
									</button>
									<SessionManagementToolbar
										active={management.managing}
										allSelected={management.allSelected}
										hasItems={sessions.length > 0}
										selectedCount={management.count}
										busy={management.busy}
										onToggleAll={management.toggleAll}
										onDelete={management.count > 0 ? () => management.requestDelete() : undefined}
										onFinish={() => {
											setMenu(null);
											management.finish();
											topRef.current?.querySelector<HTMLElement>(".sidebar-new")?.focus();
										}}
									/>
								</div>
								{recentOpen && (
									<div className="sidebar-recent-list">
										{sessions.map((session, index) => (
											<SessionEntry
												key={session.id}
												session={session}
												index={index}
												active={
													location.pathname === `/s/${session.id}` ||
													location.pathname.startsWith(`/s/${session.id}/`)
												}
												managing={management.managing}
												selected={management.selection?.has(session.id) ?? false}
												disabled={management.busy}
												preview={preview}
												showMoreButton
												menuOpen={actions.menu?.id === session.id}
												viewTransition={
													location.pathname.startsWith("/settings") ||
													location.pathname.endsWith("/export")
												}
												onToggle={() => {
													setMenu(null);
													management.toggle(session.id);
												}}
												onNavigate={() => onNavigate?.()}
												onOpenMenu={(x, y, toggle) => openSessionMenu(session, x, y, toggle)}
											/>
										))}
										{!sessions.length && (state.stage || preview) && (
											<p className="sidebar-empty">暂无会话</p>
										)}
									</div>
								)}
							</div>
						</>
					)}
					{state.error && (
						<p className="sidebar-error" role="alert">
							{state.error}
						</p>
					)}
				</div>
			</div>
			<SessionActionsOverlay actions={actions} />

			{/* 3. 底部账号与设置菜单栏（fMdf9，高 36px，整行统一作为 Popover 触发器确保 React Aria 焦距与外部点击完全自洽） */}
			<div className="sidebar-footer">
				<Popover
					isOpen={accountMenuOpen}
					onOpenChange={(open) => {
						setAccountMenuOpen(open);
						if (open) void refreshCstoaAccount();
					}}
				>
					<Button
						ref={accountTriggerRef}
						variant="ghost"
						className="sidebar-footer-row sidebar-account-btn sidebar-account-trigger"
						aria-label={`账号与设置菜单：${accountDisplayName}`}
						data-open={accountMenuOpen || undefined}
					>
						<span className="sidebar-avatar" aria-hidden="true">
							{accountInitial ? accountInitial : <User size={13} />}
						</span>
						<span className="sidebar-account-label">{accountDisplayName}</span>
						<span className="sidebar-footer-gear" aria-hidden="true">
							<Settings size={18} />
						</span>
					</Button>
					<Popover.Content
						placement="top end"
						offset={8}
						containerPadding={12}
						className="tier-2-glass tier-2-menu sidebar-account-popover"
					>
						<Popover.Dialog aria-label="账号菜单" className="tier-2-menu-list sidebar-account-menu">
							{(cstoaSummary || preview) && (
								<>
									<div className="sidebar-account-identity">
										<strong title={preview ? "示例账号 · 设计预览" : cstoaSummary?.identity}>
											{preview ? "示例账号 · 设计预览" : cstoaSummary?.identity}
										</strong>
									</div>
									<hr className="menu-sep" />
								</>
							)}
							<Link className="menu-row" to="/account" viewTransition onClick={closeAccountMenu}>
								<CircleUserRound size={16} aria-hidden="true" />
								<span>账号信息</span>
							</Link>
							{(!teamSignedIn || teamRequiresLogin) && (
								<Link
									className="menu-row"
									to={loginPath("cstoa", "oauth", "/account")}
									viewTransition={location.pathname.endsWith("/export")}
									onClick={closeAccountMenu}
								>
									<LogIn size={16} aria-hidden="true" />
									<span>{teamRequiresLogin ? "重新登录 CSTOA" : "登录 CSTOA"}</span>
								</Link>
							)}
							<Link className="menu-row" to="/settings" viewTransition onClick={closeAccountMenu}>
								<Settings size={16} aria-hidden="true" />
								<span>设置</span>
							</Link>
							<button
								type="button"
								className="menu-row"
								disabled={accountWorking}
								aria-label={`主题，当前${themeLabels[theme]}，切换为${themeLabels[nextTheme[theme]]}`}
								onClick={() => void changeTheme()}
							>
								<SunMoon size={16} aria-hidden="true" />
								<span>主题</span>
								<small className="menu-row-suffix">{themeLabels[theme]}</small>
							</button>
							<a
								className="menu-row"
								href="https://github.com/SCAU-Computer-Serving-Team/cst-pilot/issues/new"
								onClick={closeAccountMenu}
								target="_blank"
								rel="noopener noreferrer"
							>
								<MessageSquareWarning size={16} aria-hidden="true" />
								<span>反馈问题</span>
							</a>
							<hr className="menu-sep" />
							<button
								type="button"
								className="menu-row danger"
								onClick={() => {
									setAccountMenuOpen(false);
									setExitOpen(true);
								}}
							>
								<Power size={16} aria-hidden="true" />
								<span>退出 CST Pilot</span>
							</button>
							{accountError && (
								<p role="alert" className="sidebar-error">
									{accountError}
								</p>
							)}
						</Popover.Dialog>
					</Popover.Content>
				</Popover>
			</div>
			{exitOpen && <ExitDialog onClose={() => setExitOpen(false)} />}
		</aside>
	);
}

export function BrandArcs() {
	return (
		<svg className="brand-arcs" viewBox="0 0 280 280" fill="none" aria-hidden="true">
			<defs>
				<linearGradient id="brand-arc-gradient" x1="0%" y1="100%" x2="0%" y2="0%">
					<stop offset="0%" stopColor="#fff" stopOpacity="0" />
					<stop offset="55%" stopColor="#fff" stopOpacity="0.12" />
					<stop offset="100%" stopColor="#fff" stopOpacity="1" />
				</linearGradient>
			</defs>
			<path
				d="M 237.76 237.76 A 138.25 138.25 0 1 1 237.76 42.24"
				stroke="url(#brand-arc-gradient)"
				strokeWidth="3.5"
			/>
			<path
				d="M 225.03 225.03 A 120.25 120.25 0 1 1 225.03 54.97"
				stroke="url(#brand-arc-gradient)"
				strokeWidth="3.5"
			/>
		</svg>
	);
}

export function HomeSurface({
	preview = false,
	paused = false,
	externalBackground = false,
}: {
	preview?: boolean;
	paused?: boolean;
	externalBackground?: boolean;
}) {
	const { outbox, records } = useMessageOutbox();
	// 首页只保留需要处理的发送记录；正常气泡由接管后的聊天页展示。
	const outgoing = preview
		? []
		: outgoingFor(records).filter((item) => item.state === "failed" || item.state === "unknown");
	async function send(text: string, config: ComposerConfig) {
		const { messageId, ...payload } = config;
		outbox.enqueue({ ...payload, id: messageId, text });
	}
	return (
		<main className={`home-main ${externalBackground ? "home-main--external-background" : ""}`}>
			{!externalBackground && (
				<div className="home-backdrop" aria-hidden="true">
					<BlueHour kind="home" paused={paused} />
				</div>
			)}
			<div className="home-center">
				<div className="home-greeting">
					<BrandArcs />
					<h1>我该如何协助您？</h1>
				</div>
				{!!outgoing.length && (
					<section className="home-outgoing" aria-label="待发送消息">
						{outgoing.map((item) => (
							<UserMessage
								key={item.id}
								outgoing={item}
								message={{
									role: "user",
									content: item.text,
									timestamp: item.createdAt,
									clientMessageId: item.id,
								}}
							/>
						))}
					</section>
				)}
				<Composer home onSend={preview || paused ? undefined : send} />
			</div>
			<span className="home-footnote">@cst-pilot</span>
		</main>
	);
}
