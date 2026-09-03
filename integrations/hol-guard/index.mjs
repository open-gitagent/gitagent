import { spawn } from "node:child_process";

const DEFAULT_TIMEOUT_MS = 6000;

function nonEmptyString(value) {
	return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function guardReason(payload) {
	const hookSpecific = payload?.hookSpecificOutput;
	for (const value of [
		hookSpecific?.permissionDecisionReason,
		payload?.reason,
		payload?.stopReason,
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

	const permissionDecision = payload.hookSpecificOutput?.permissionDecision;
	if (permissionDecision === "allow") return { action: "allow" };
	if (permissionDecision === "deny") {
		return { action: "block", reason: guardReason(payload) };
	}
	if (permissionDecision === "ask") {
		return {
			action: "block",
			reason: guardReason(payload) || "HOL Guard requires review before this command can run.",
		};
	}

	const decision = nonEmptyString(payload.decision)?.toLowerCase();
	if (decision === "allow") return { action: "allow" };
	if (decision === "block" || decision === "deny" || decision === "ask" || decision === "review") {
		return { action: "block", reason: guardReason(payload) };
	}

	const policyAction = nonEmptyString(payload.policy_action)?.toLowerCase();
	if (policyAction === "allow" || policyAction === "warn") return { action: "allow" };
	if (["block", "review", "require-reapproval", "sandbox-required"].includes(policyAction)) {
		return { action: "block", reason: guardReason(payload) };
	}

	return { action: "block", reason: "HOL Guard returned no recognized decision." };
}

function lastJsonObject(stdout) {
	const lines = stdout.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
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

function guardArgs(config) {
	const args = ["guard", "hook"];
	const guardHome = nonEmptyString(config.guard_home);
	const home = nonEmptyString(config.home);
	const workspace = nonEmptyString(config.workspace);
	if (guardHome) args.push("--guard-home", guardHome);
	args.push("--harness", "codex");
	if (home) args.push("--home", home);
	if (workspace) args.push("--workspace", workspace);
	args.push("--json");
	return args;
}

export async function evaluateWithGuard(ctx, config = {}) {
	if (ctx?.tool !== "cli") return { action: "allow" };
	const command = nonEmptyString(ctx?.args?.command);
	if (!command) return { action: "allow" };

	const binary = nonEmptyString(config.binary) || process.env.HOL_GUARD_BIN || "hol-guard";
	const configuredTimeout = Number(config.timeout_ms);
	const timeoutMs = Number.isFinite(configuredTimeout) && configuredTimeout > 0
		? configuredTimeout
		: DEFAULT_TIMEOUT_MS;
	const workspace = nonEmptyString(config.workspace) || process.cwd();
	const input = JSON.stringify({
		hook_event_name: "PreToolUse",
		event: "PreToolUse",
		session_id: ctx.session_id,
		tool_name: "Bash",
		tool_input: { command },
		cwd: workspace,
	});

	return new Promise((resolve) => {
		let stdout = "";
		let stderr = "";
		let settled = false;
		const child = spawn(binary, guardArgs({ ...config, workspace }), {
			stdio: ["pipe", "pipe", "pipe"],
			env: { ...process.env },
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
		child.stdin.on("error", () => {});
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
			const payload = lastJsonObject(stdout);
			finish(guardResponseToHookResult(payload));
		});

		child.stdin.end(input);
	});
}

export async function register(api) {
	api.registerHook("pre_tool_use", async (ctx) => evaluateWithGuard(ctx, api.config));
}
