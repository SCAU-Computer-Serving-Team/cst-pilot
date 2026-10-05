import { useCallback, useEffect, useRef, useState } from "react";
import { cancelOauthLogin, getOauthStatus, type OauthStatus, startOauthLogin } from "../data/api";
import { useGlobalEvents } from "../data/web-state";
import { acceptsOAuthStatus } from "./oauth-state";

export function useOAuth(
	providerId: string,
	onSuccess: () => void,
	method: "oauth" | "api_key" = "oauth",
	initialBody?: { key?: string; baseUrl?: string },
) {
	const seed = useRef(initialBody);
	const [status, setStatus] = useState<OauthStatus>({ state: "idle" });
	const [busy, setBusy] = useState(false);
	const [connectionError, setConnectionError] = useState("");
	const [expiresAt, setExpiresAt] = useState(0);
	const mounted = useRef(false);
	const activeProvider = useRef(providerId);
	activeProvider.current = providerId;
	const succeeded = useRef("");
	const activeFlow = useRef("");
	const acceptedStatus = useRef<OauthStatus>({ state: "idle" });
	const device = useRef({ key: "", expiresAt: 0 });
	const done = useRef(onSuccess);
	done.current = onSuccess;
	const accept = useCallback(
		(value: OauthStatus) => {
			if (
				!mounted.current ||
				activeProvider.current !== providerId ||
				(value.authType !== undefined && value.authType !== method) ||
				!acceptsOAuthStatus(acceptedStatus.current, value, activeFlow.current)
			)
				return;
			acceptedStatus.current = value;
			setStatus(value);
			setConnectionError("");
			if (value.deviceCode) {
				const key = `${value.flowId}:${value.deviceCode.userCode}`;
				const deadline = Date.now() + value.deviceCode.expiresInSeconds * 1000;
				device.current = {
					key,
					expiresAt: device.current.key === key ? Math.min(device.current.expiresAt, deadline) : deadline,
				};
				setExpiresAt(device.current.expiresAt);
			}
			if (value.state === "succeeded" && value.flowId !== succeeded.current) {
				succeeded.current = value.flowId ?? "";
				done.current();
			}
		},
		[providerId, method],
	);
	const sync = useCallback(async () => {
		if (!activeFlow.current) return;
		try {
			accept(await getOauthStatus(providerId, method));
		} catch (error) {
			if (mounted.current)
				setConnectionError(error instanceof Error ? error.message : "无法读取登录进度，请检查连接。");
		}
	}, [providerId, method, accept]);
	useEffect(() => {
		mounted.current = true;
		let disposed = false;
		setStatus({ state: "idle" });
		activeFlow.current = "";
		acceptedStatus.current = { state: "idle" };
		device.current = { key: "", expiresAt: 0 };
		succeeded.current = "";
		void startOauthLogin(providerId, method, seed.current)
			.then((value) => {
				if (
					disposed &&
					value.state === "pending" &&
					value.flowId &&
					(!mounted.current || activeProvider.current !== providerId)
				) {
					void cancelOauthLogin(providerId, value.flowId, method).catch(() => {});
				}
				if (!disposed) {
					seed.current = undefined;
					activeFlow.current = value.flowId ?? "";
					accept(value);
				}
			})
			.catch((error: unknown) => {
				if (!disposed)
					setStatus({ state: "failed", error: error instanceof Error ? error.message : "无法开始登录，请重试。" });
			});
		let polling = false;
		const timer = window.setInterval(() => {
			if (polling || disposed) return;
			polling = true;
			void sync().finally(() => {
				polling = false;
			});
		}, 2000);
		return () => {
			disposed = true;
			mounted.current = false;
			window.clearInterval(timer);
			const flowId = activeFlow.current;
			if (flowId)
				queueMicrotask(() => {
					if (!mounted.current || activeProvider.current !== providerId)
						void cancelOauthLogin(providerId, flowId, method).catch(() => {});
				});
		};
	}, [providerId, method, accept, sync]);
	useGlobalEvents((name, raw) => {
		if (name === "open" || name === "reset") {
			void sync();
			return;
		}
		if (name !== "state") return;
		try {
			const event = JSON.parse(raw?.data ?? "null");
			if (event?.type === "oauth_state" && event.providerId === providerId) accept(event);
		} catch {
			/* 畸形事件由下一次状态查询恢复。 */
		}
	});
	async function restart() {
		if (busy) return;
		setBusy(true);
		const previousFlow = activeFlow.current;
		activeFlow.current = "";
		acceptedStatus.current = { state: "idle" };
		try {
			await cancelOauthLogin(providerId, previousFlow || status.flowId, method);
			device.current = { key: "", expiresAt: 0 };
			setExpiresAt(0);
			setStatus({ state: "idle" });
			const value = await startOauthLogin(providerId, method);
			activeFlow.current = value.flowId ?? "";
			accept(value);
		} catch (error) {
			if (mounted.current)
				setStatus({ state: "failed", error: error instanceof Error ? error.message : "无法重新登录，请重试。" });
		} finally {
			if (mounted.current) setBusy(false);
		}
	}
	async function cancel() {
		if (busy) return;
		setBusy(true);
		try {
			await cancelOauthLogin(providerId, status.flowId, method);
			await sync();
		} catch (error) {
			if (mounted.current) setConnectionError(error instanceof Error ? error.message : "取消登录失败，请重试。");
		} finally {
			if (mounted.current) setBusy(false);
		}
	}
	return { status, busy, expiresAt, connectionError, restart, cancel, sync };
}
