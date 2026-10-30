import { index, layout, type RouteConfig, route } from "@react-router/dev/routes";

export default [
	route("login", "routes/login.tsx"),
	route("_export", "routes/export-render.tsx"),
	layout("routes/layout.tsx", [
		index("routes/home.tsx"),
		route("sessions", "routes/sessions.tsx"),
		route("s/:sessionId", "routes/chat.tsx"),
		route("s/:sessionId/tree", "routes/tree.tsx"),
		route("s/:sessionId/export", "routes/export.tsx"),
		route("settings", "routes/settings.tsx", [
			index("routes/settings-services.tsx"),
			route("general", "routes/settings-general.tsx"),
			route("models", "routes/settings-models.tsx"),
			route("shortcuts", "routes/settings-shortcuts.tsx"),
			route("add", "routes/settings-add.tsx"),
			route("custom", "routes/settings-custom.tsx"),
			route("provider", "routes/provider.tsx"),
		]),
		route("account", "routes/account.tsx"),
	]),
] satisfies RouteConfig;
