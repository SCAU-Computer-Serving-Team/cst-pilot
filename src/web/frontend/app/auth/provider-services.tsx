import { ChevronRight, TriangleAlert } from "lucide-react";
import { type KeyboardEvent as ReactKeyboardEvent, useCallback, useRef, useState } from "react";
import { describeAccount } from "../account/format";
import { useAccount } from "../account/use-account";
import type { ProviderStatus } from "../data/api";
import { providerDescription } from "../settings/provider-list";
import { querySettingsRouteStops, SettingsLink } from "../settings/settings-frame";
import { KbdFocusOverlay, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
import { resolveListNavKey } from "../shell/keyboard-navigation";
import { configurationPath, initialLogin } from "./providers";

export function ProviderServices({
	providers,
	adding = false,
	unframed = false,
}: {
	providers: ProviderStatus[];
	adding?: boolean;
	unframed?: boolean;
}) {
	const { account } = useAccount();
	const identity = describeAccount(account);
	const listRef = useRef<HTMLDivElement>(null);
	const itemRefs = useRef<Map<string, HTMLElement>>(new Map());
	const kbdStyle = useKeyboardFocusStyle();
	const [activeId, setActiveId] = useState<string | null>(null);
	const [kbdActive, setKbdActive] = useState(false);
	const clearKbd = useCallback(() => setKbdActive(false), []);
	usePointerMutex(kbdActive, clearKbd);

	function handleKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
		const links = [...(listRef.current?.querySelectorAll<HTMLAnchorElement>(".settings-service-row a") ?? [])];
		if (!links.length) return;
		const cur = Math.max(0, links.indexOf(document.activeElement as HTMLAnchorElement));
		if (
			cur === 0 &&
			event.key === "ArrowUp" &&
			!event.defaultPrevented &&
			!event.nativeEvent.isComposing &&
			event.keyCode !== 229 &&
			!event.altKey &&
			!event.ctrlKey &&
			!event.metaKey &&
			!event.shiftKey
		) {
			const route = listRef.current?.closest<HTMLElement>(".settings-route");
			if (route) {
				const focusables = querySettingsRouteStops(route);
				const pos = focusables.indexOf(links[0]);
				const prev =
					pos > 0
						? focusables[pos - 1]
						: document.querySelector<HTMLElement>(
								'.settings-nav-link.is-current, .settings-nav-link[tabindex="0"]',
							);
				if (prev) {
					event.preventDefault();
					setActiveId(null);
					setKbdActive(false);
					prev.focus();
					return;
				}
			}
		}
		if (
			cur + 1 >= links.length &&
			event.key === "ArrowDown" &&
			!event.defaultPrevented &&
			!event.nativeEvent.isComposing &&
			event.keyCode !== 229 &&
			!event.altKey &&
			!event.ctrlKey &&
			!event.metaKey &&
			!event.shiftKey
		) {
			const route = listRef.current?.closest<HTMLElement>(".settings-route");
			const lastLink = links[links.length - 1];
			if (route && lastLink) {
				const focusables = querySettingsRouteStops(route);
				const pos = focusables.indexOf(lastLink);
				const nextStop = pos >= 0 ? focusables[pos + 1] : undefined;
				if (nextStop) {
					event.preventDefault();
					setActiveId(null);
					setKbdActive(false);
					nextStop.focus();
					return;
				}
			}
		}
		const nextIdx = resolveListNavKey(
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
			cur,
			links.length,
		);
		if (nextIdx !== null) {
			event.preventDefault();
			setKbdActive(true);
			const target = providers[nextIdx];
			if (target) setActiveId(target.id);
			links[nextIdx]?.focus();
		}
	}

	return (
		<div
			ref={listRef}
			role="listbox"
			aria-label={adding ? "可添加的提供商" : "已配置提供商"}
			className={
				unframed ? "settings-services settings-flat-list" : "settings-card settings-services settings-flat-list"
			}
			style={{ position: "relative" }}
			data-kbd-active={kbdActive && activeId !== null ? "true" : undefined}
			onKeyDown={handleKeyDown}
			onBlur={(event) => {
				if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
					setActiveId(null);
					setKbdActive(false);
				}
			}}
		>
			{providers.map((item) => {
				const provider = item.id === "cstoa" && account?.requiresLogin ? { ...item, requiresLogin: true } : item;
				const initial = initialLogin([provider], provider.id, provider.type);
				const status = providerDescription(provider);
				const actionLabel = adding ? "连接" : provider.requiresLogin ? "重新登录" : "管理";
				const targetHref = provider.custom
					? `/settings/custom?provider=${encodeURIComponent(provider.id)}`
					: configurationPath(provider.id, initial?.method ?? "api_key");
				return (
					<div
						ref={(node) => {
							if (node) itemRefs.current.set(provider.id, node);
							else itemRefs.current.delete(provider.id);
						}}
						className={`settings-flat-row settings-service-row${adding ? " settings-service-row--compact" : ""}`}
						key={provider.id}
						data-provider={provider.id}
					>
						{adding ? (
							<div className="settings-row-inline">
								<span className="settings-service-name">{provider.name}</span>
								<span className="settings-description">{status}</span>
							</div>
						) : (
							<div className="settings-row-copy">
								<span className="settings-service-name">{provider.name}</span>
								<span className="settings-description" data-warning={provider.requiresLogin || undefined}>
									{provider.requiresLogin && <TriangleAlert size={13} aria-hidden="true" />}
									{provider.id === "cstoa" && identity && !provider.requiresLogin
										? `${identity.identity} · ${status}`
										: status}
								</span>
							</div>
						)}
						<SettingsLink
							className="settings-inline-action"
							to={targetHref}
							aria-label={`${actionLabel} ${provider.name}`}
							onFocus={(event) => {
								setActiveId(provider.id);
								if (event.currentTarget.matches(":focus-visible")) setKbdActive(true);
							}}
							onClick={() => setKbdActive(false)}
						>
							{actionLabel}
							<ChevronRight size={14} aria-hidden="true" />
						</SettingsLink>
					</div>
				);
			})}
			<KbdFocusOverlay
				containerRef={listRef}
				itemRefs={itemRefs}
				targetId={kbdActive && activeId && providers.some((p) => p.id === activeId) ? activeId : null}
				style={kbdStyle}
				railPlacement="outside"
				bracketOutset={4}
			/>
		</div>
	);
}
