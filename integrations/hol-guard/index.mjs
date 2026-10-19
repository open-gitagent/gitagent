import { spawn } from "node:child_process";

const DEFAULT_TIMEOUT_MS = 6000;

function nonEmptyString(value) {
	return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function guardReason(payload) {
	for (const value of [
		payload?.classification?.reason,
		payload?.reason,
		payload?.message,
	]) {
		const text = nonEmptyString(value);
		if (text) return text;
	}
	return "HOL Guard did not allow this command.";
}

export function guardResponseToHookResult(payload) {
	if (!payload || typeof payload !== "object") {
		return { action: "block", reason: "HOL Guard returned an invalid response." };
	}

	const minimumAction = nonEmptyString(payload.minimum_action)?.toLowerCase();
	const explicitlyBenign = payload?.classification?.explicitly_benign === true;
	if (minimumAction === "allow" && explicitlyBenign) {
		return { action: "allow" };
	}
	if (["allow", "monitor", "review", "block"].includes(minimumAction)) {
		return { action: "block", reason: guardReason(payload) };
	}

	return { action: "block", reason: "HOL Guard returned no recognized decision." };
}

function guardJsonObject(stdout) {
	const output = stdout.trim();
	if (!output) return null;
	try {
		const value = JSON.parse(output);
		if (value && typeof value === "object" && !Array.isArray(value)) return value;
	} catch {
		// Fall back to a final standalone JSON line for wrappers that add diagnostics.
	}

	const lines = output.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
	for (let i = lines.length - 1; i >= 0; i--) {
		try {
			const value = JSON.parse(lines[i]);
			if (value && typeof value === "object" && !Array.isArray(value)) return value;
		} catch {
			// Continue scanning in case HOL Guard emitted a diagnostic line first.
		}
	}
	return null;
}

function guardArgs(command) {
	return ["command", "test", "--json", "--", command];
}

export async function evaluateWithGuard(ctx, config = {}) {
	if (ctx?.tool !== "cli") return { action: "allow" };
	const command = nonEmptyString(ctx?.args?.command);
	if (!command) return { action: "block", reason: "HOL Guard could not inspect a cli command." };

	const binary = nonEmptyString(config.binary) || process.env.HOL_GUARD_BIN || "hol-guard";
	const configuredTimeout = Number(config.timeout_ms);
	const timeoutMs = Number.isFinite(configuredTimeout) && configuredTimeout > 0
		? configuredTimeout
		: DEFAULT_TIMEOUT_MS;
	const workspace = nonEmptyString(config.workspace) || process.cwd();
	const childEnv = { ...process.env };
	const guardHome = nonEmptyString(config.guard_home);
	if (guardHome) childEnv.HOL_GUARD_HOME = guardHome;
	const home = nonEmptyString(config.home);
	if (home) childEnv.HOME = home;

	return new Promise((resolve) => {
		let stdout = "";
		let stderr = "";
		let settled = false;
		const child = spawn(binary, guardArgs(command), {
			stdio: ["ignore", "pipe", "pipe"],
			env: childEnv,
			cwd: workspace,
			shell: false,
		});

		const finish = (result) => {
			if (settled) return;
			settled = true;
			clearTimeout(timer);
			resolve(result);
		};

		const timer = setTimeout(() => {
			child.kill("SIGTERM");
			finish({
				action: "block",
				reason: `HOL Guard did not return a decision within ${timeoutMs}ms.`,
			});
		}, timeoutMs);

		child.stdout.on("data", (chunk) => { stdout += chunk.toString("utf-8"); });
		child.stderr.on("data", (chunk) => { stderr += chunk.toString("utf-8"); });
		child.on("error", (error) => {
			finish({ action: "block", reason: `HOL Guard could not start: ${error.message}` });
		});
		child.on("close", (code) => {
			if (settled) return;
			if (code !== 0) {
				finish({
					action: "block",
					reason: nonEmptyString(stderr) || `HOL Guard exited with code ${code}.`,
				});
				return;
			}
			finish(guardResponseToHookResult(guardJsonObject(stdout)));
		});
	});
}

export async function register(api) {
	api.registerHook("pre_tool_use", async (ctx) => evaluateWithGuard(ctx, api.config));
}
