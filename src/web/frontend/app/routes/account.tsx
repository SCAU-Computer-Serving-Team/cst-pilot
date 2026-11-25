import { CircleUserRound } from "lucide-react";
import { useEffect } from "react";
import { AccountView } from "../account/account-view";
import { useAccount } from "../account/use-account";
import { SidebarHeaderToggle } from "../shell/workspace-shell-context";

export default function Account() {
	const state = useAccount();
	useEffect(() => {
		void state.refresh();
	}, [state.refresh]);
	return (
		<main className="chat-main settings-main">
			<header className="chat-header settings-header">
				<SidebarHeaderToggle />
				<CircleUserRound size={16} aria-hidden="true" />
				<h1>账号信息</h1>
			</header>
			<AccountView {...state} onRetry={() => void state.refresh(true)} onLogout={() => void state.logout()} />
		</main>
	);
}
