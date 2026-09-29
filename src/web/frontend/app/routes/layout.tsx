import { Outlet, useLocation, useSearchParams } from "react-router";
import { Sidebar } from "../app/shell";

/** 工作台外壳：侧栏只挂载一次，路由切换时不再重建，避免会话列表闪空。 */
export default function WorkspaceLayout() {
  const { pathname } = useLocation();
  const [params] = useSearchParams();
  const preview = params.get("preview") === "1";
  const home = pathname === "/";
  return (
    <div className={`app-layout ${home ? "home-layout" : "chat-layout"}`}>
      <Sidebar preview={preview} />
      <Outlet />
    </div>
  );
}
