/** CSTOA 明确要求重新授权；跨 Pi 包装和扩展重载按错误码识别。 */
export class CstoaLoginRequired extends Error {
	readonly code = "cstoa_login_required";
}

export function requiresCstoaLogin(error: unknown): boolean {
	const seen = new Set<unknown>();
	while (error && typeof error === "object" && !seen.has(error)) {
		seen.add(error);
		if ("code" in error && error.code === "cstoa_login_required") return true;
		error = "cause" in error ? error.cause : undefined;
	}
	return false;
}
