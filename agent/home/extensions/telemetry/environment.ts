import { execFile } from "node:child_process";
import { createHmac } from "node:crypto";
import { existsSync } from "node:fs";
import { machine, release, type } from "node:os";
import { resolve } from "node:path";
import { promisify } from "node:util";
import { loadInstallation } from "./installation.ts";

export interface SystemProbe {
	machineGuid?: string | null;
	name?: string | null;
	displayVersion?: string | null;
	arch?: string | null;
	isPE?: boolean | null;
	admin?: boolean | null;
}

export interface EnvironmentSnapshot {
	identity: {
		installationId: string | null;
		environmentId: string | null;
		environmentSource: "windows-machine-guid" | "unavailable";
	};
	os: {
		platform: string;
		name: string | null;
		version: string;
		displayVersion: string | null;
		arch: string | null;
		isPE: boolean | null;
	};
	runtime: { version: string; arch: string };
	admin: boolean | null;
}

const architectures: Record<string, string> = {
	amd64: "x64",
	x86_64: "x64",
	x64: "x64",
	arm64: "arm64",
	aarch64: "arm64",
	x86: "ia32",
	i686: "ia32",
};
function architecture(value: unknown): string | null {
	return typeof value === "string" ? (architectures[value.toLowerCase()] ?? null) : null;
}

export function unknownEnvironment(): EnvironmentSnapshot {
	return {
		identity: { installationId: null, environmentId: null, environmentSource: "unavailable" },
		os: { platform: process.platform, name: null, version: release(), displayVersion: null, arch: null, isPE: null },
		runtime: { version: process.versions.node, arch: process.arch },
		admin: null,
	};
}

const execute = promisify(execFile);
async function probeSystem(agentDir: string): Promise<SystemProbe> {
	if (process.platform !== "win32") return { name: type(), arch: architecture(machine()), isPE: false };
	const portable = resolve(agentDir, "..", "..", "pwsh", "pwsh.exe");
	const executable = existsSync(portable) ? portable : "powershell.exe";
	const script = `
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false)
$OutputEncoding=[Console]::OutputEncoding
$r=@{name=$null;displayVersion=$null;arch=$null;isPE=$null;admin=$null;machineGuid=$null}
try { $r.name=(Get-CimInstance Win32_OperatingSystem -Property Caption).Caption } catch {}
try { $r.displayVersion=(Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion').DisplayVersion } catch {}
try { $r.arch=[Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITEW6432'); if (-not $r.arch) { $r.arch=$env:PROCESSOR_ARCHITECTURE } } catch {}
try { $r.isPE=Test-Path 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\MiniNT' } catch {}
try { $r.admin=([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator) } catch {}
if ($r.isPE -eq $false) { try { $r.machineGuid=(Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Cryptography').MachineGuid } catch {} }
$r | ConvertTo-Json -Compress
`;
	try {
		const { stdout } = await execute(executable, ["-NoProfile", "-NonInteractive", "-Command", script], {
			timeout: 10_000,
			maxBuffer: 32 * 1024,
			windowsHide: true,
			encoding: "utf8",
		});
		const parsed = JSON.parse(stdout.trim());
		return { ...parsed, arch: architecture(parsed.arch) };
	} catch {
		return {};
	}
}

/** 仅在会话启动发起；返回值不含原始 MachineGuid、账户信息或本地密钥。 */
export async function captureEnvironment(
	agentDir: string,
	probe: () => Promise<SystemProbe> = () => probeSystem(agentDir),
): Promise<EnvironmentSnapshot> {
	const snapshot = unknownEnvironment();
	const [installation, system] = await Promise.all([
		loadInstallation(agentDir),
		probe().catch(() => ({}) as SystemProbe),
	]);
	snapshot.identity.installationId = installation?.installationId ?? null;
	const text = (value: unknown): string | null => (typeof value === "string" && value.trim() ? value.trim() : null);
	snapshot.os.name = text(system.name);
	snapshot.os.displayVersion = text(system.displayVersion);
	snapshot.os.arch = architecture(system.arch);
	snapshot.os.isPE = typeof system.isPE === "boolean" ? system.isPE : null;
	snapshot.admin = typeof system.admin === "boolean" ? system.admin : null;
	const guid = text(system.machineGuid);
	if (installation && guid && system.isPE === false) {
		snapshot.identity.environmentId = createHmac("sha256", Buffer.from(installation.identityKey, "base64"))
			.update(JSON.stringify(["environment", guid.toLowerCase()]))
			.digest("hex");
		snapshot.identity.environmentSource = "windows-machine-guid";
	}
	return snapshot;
}
