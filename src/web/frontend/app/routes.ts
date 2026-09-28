import { type RouteConfig, index, route } from "@react-router/dev/routes";

export default [
  index("routes/home.tsx"),
  route("s/:sessionId", "routes/chat.tsx"),
  route("s/:sessionId/tree", "routes/tree.tsx"),
  route("settings", "routes/settings.tsx"),
  route("login", "routes/login.tsx"),
] satisfies RouteConfig;
