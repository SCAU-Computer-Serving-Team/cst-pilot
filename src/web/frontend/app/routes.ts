import { type RouteConfig, index, layout, route } from "@react-router/dev/routes";

export default [
  route("login", "routes/login.tsx"),
  layout("routes/layout.tsx", [
    index("routes/home.tsx"),
    route("s/:sessionId", "routes/chat.tsx"),
    route("s/:sessionId/tree", "routes/tree.tsx"),
    route("settings", "routes/settings.tsx"),
  ]),
] satisfies RouteConfig;
