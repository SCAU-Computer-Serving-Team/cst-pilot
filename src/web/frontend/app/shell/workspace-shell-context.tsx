import { PanelLeft } from "lucide-react";
import { createContext, type ReactNode, type RefObject, useContext } from "react";

export type WorkspaceShellState = {
	collapsed: boolean;
	overlay: boolean;
	appActive: boolean;
	expandSidebar: () => void;
	expandButtonRef: RefObject<HTMLButtonElement | null>;
};

const WorkspaceShellContext = createContext<WorkspaceShellState | null>(null);

export function WorkspaceShellProvider({ value, children }: { value: WorkspaceShellState; children: ReactNode }) {
	return <WorkspaceShellContext value={value}>{children}</WorkspaceShellContext>;
}

export function useWorkspaceShell(): WorkspaceShellState | null {
	return useContext(WorkspaceShellContext);
}

/**
 * Web 收起态非主页页头内联展开按钮 + 1×14px 竖分隔线（对齐 cst-pilot-web.pen `MKlBE`）。
 * 网页端折叠态仅保留展开侧栏按钮，不在顶栏承载新建对话。
 */
export function SidebarHeaderToggle() {
	const shell = useWorkspaceShell();
	if (!shell?.collapsed) return null;
	return (
		<span className="header-sidebar-toggle">
			<button
				ref={shell.expandButtonRef}
				type="button"
				className="btn-icon sm-28 sidebar-restore-inline"
				aria-label="展开侧栏"
				title="展开侧栏"
				onClick={shell.expandSidebar}
			>
				<PanelLeft size={16} aria-hidden="true" />
			</button>
			<span className="header-divider" aria-hidden="true" />
		</span>
	);
}
