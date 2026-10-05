import { CircleUserRound } from "lucide-react";
import { AccountView } from "../account/account-view";
import { useAccount } from "../account/use-account";

export default function Account() {
	const state = useAccount();
	return (
		<main className="chat-main settings-main">
			<header className="chat-header settings-header">
				<CircleUserRound size={22} aria-hidden="true" />
				<h1>账号信息</h1>
			</header>
			<AccountView {...state} onRetry={() => void state.refresh()} onLogout={() => void state.logout()} />
		</main>
	);
}
