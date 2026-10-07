export type Theme = "system" | "light" | "dark";
export const themeLabels: Record<Theme, string> = { system: "跟随系统", light: "浅色", dark: "深色" };
export const nextTheme: Record<Theme, Theme> = { system: "light", light: "dark", dark: "system" };
export const currentTheme = (): Theme => {
	const value = document.documentElement.dataset.theme;
	return value === "dark" || value === "system" ? value : "light";
};
