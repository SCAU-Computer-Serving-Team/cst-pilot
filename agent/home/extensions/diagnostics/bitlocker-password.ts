export function validateRecoveryPassword(value: string): string {
	const text = value.trim();
	if (!/^\d{6}(?:-\d{6}){7}$/.test(text)) throw new Error("恢复密码必须为八组六位数字，用连字符分隔");
	return text;
}

export function redactRecoveryPasswords(value: string): string {
	return value.replace(/\d{6}(?:-\d{6}){7}|(?<!\d)\d{48}(?!\d)/g, "[BitLocker 恢复密码已隐藏]");
}
