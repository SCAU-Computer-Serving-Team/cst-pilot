import { type RouteConfig, index, route } from "@react-router/dev/routes";

export default [
  index("routes/home.tsx"),
  route("s/:sessionId", "routes/chat.tsx"),
  route("login", "routes/login.tsx"),
] satisfies RouteConfig;
