import assert from "node:assert/strict";
import { test } from "node:test";
import { derivePythonIssues } from "./python-core.ts";

const command = (name: string, path: string) => ({ name, path });

test("reports multiple python commands and manager overlaps", () => {
	const issues = derivePythonIssues(
		[
			command("python", "C:\\Python312\\python.exe"),
			command("python3", "C:\\Python313\\python.exe"),
			command("conda", "C:\\miniconda3\\Scripts\\conda.exe"),
			command("uv", "C:\\Users\\me\\.cargo\\bin\\uv.exe"),
		],
		[],
		{},
	);
	assert.ok(issues.some((issue) => issue.code === "multiple-python-commands"));
	assert.ok(issues.some((issue) => issue.code === "multiple-package-managers"));
});

test("reports anaconda and miniconda coexistence", () => {
	const issues = derivePythonIssues(
		[command("conda", "C:\\Anaconda3\\Scripts\\conda.exe"), command("python", "C:\\miniconda3\\python.exe")],
		[],
		{ conda: { root_prefix: "C:\\miniconda3", envs: [] } },
	);
	assert.ok(issues.some((issue) => issue.code === "anaconda-and-miniconda"));
});

test("reports missing active environment and conflicting active markers", () => {
	const issues = derivePythonIssues([command("python", "C:\\Python312\\python.exe")], [], {
		conda: { active_prefix: "C:\\conda\\envs\\missing" },
		environmentVariables: {
			CONDA_PREFIX: "C:\\conda\\envs\\missing",
			VIRTUAL_ENV: "C:\\project\\.venv",
		},
	});
	assert.ok(issues.some((issue) => issue.code === "active-environment-not-found"));
	assert.ok(issues.some((issue) => issue.code === "venv-and-conda-active"));
});

test("reports pymol inside conda", () => {
	const issues = derivePythonIssues(
		[command("conda", "C:\\miniconda3\\Scripts\\conda.exe"), command("pymol", "C:\\miniconda3\\Scripts\\pymol.exe")],
		[],
		{},
	);
	assert.ok(issues.some((issue) => issue.code === "pymol-in-conda"));
});
