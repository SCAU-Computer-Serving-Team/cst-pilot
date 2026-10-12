export const sessionGroups = ["今天", "昨天", "一周内", "一个月内", "更早"] as const;
export type SessionGroup = (typeof sessionGroups)[number];

/** 按本机日历日期划分互斥分组，避免跨夏令时产生一天的误差。 */
export function dayGroup(value?: string, now = new Date()): SessionGroup {
	const target = value ? new Date(value) : undefined;
	if (!target || Number.isNaN(target.getTime())) return "更早";
	const calendarDay = (date: Date) => Date.UTC(date.getFullYear(), date.getMonth(), date.getDate());
	const days = Math.round((calendarDay(now) - calendarDay(target)) / 86400000);
	return days <= 0 ? "今天" : days === 1 ? "昨天" : days <= 7 ? "一周内" : days <= 30 ? "一个月内" : "更早";
}

export function groupSelection(ids: readonly string[], selected: ReadonlySet<string> | null) {
	const count = ids.filter((id) => selected?.has(id)).length;
	return { checked: ids.length > 0 && count === ids.length, mixed: count > 0 && count < ids.length };
}

/** 全选组再次点击清空；未选或半选组点击全选。其他组保持原选择。 */
export function toggleSessionGroup(ids: readonly string[], selected: ReadonlySet<string>): Set<string> {
	const next = new Set(selected);
	const remove = groupSelection(ids, selected).checked;
	for (const id of ids) {
		if (remove) next.delete(id);
		else next.add(id);
	}
	return next;
}
