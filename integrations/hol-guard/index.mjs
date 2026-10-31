import { spawn } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { TextDecoder } from "node:util";
import { delimiter, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";

const DEFAULT_TIMEOUT_MS = 6000;
const MAX_GUARD_OUTPUT_BYTES = 1024 * 1024;

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

	// A Guard decision other than an explicit allow must never be overridden
	// by a contradictory benign classification or minimum_action field.
	const decision = nonEmptyString(payload.decision)?.toLowerCase();
	const continuationStopped = Object.prototype.hasOwnProperty.call(payload, "continue") && payload.continue !== true;
	const decisionStopped = Object.prototype.hasOwnProperty.call(payload, "decision") && decision !== "allow";
	// Never let a benign command classifier override a concrete policy stop.
	// Inspection-only output can use not_run without evaluating runtime policy.
	const hasPolicyAction = Object.prototype.hasOwnProperty.call(payload, "policy_action");
	const policyAction = nonEmptyString(payload.policy_action)?.toLowerCase();
	const policyStopped = hasPolicyAction && !["allow", "not_run"].includes(policyAction);
	const hasHookOutput = Object.prototype.hasOwnProperty.call(payload, "hookSpecificOutput");
	const hookOutput = payload.hookSpecificOutput;
	const hookStopped = hasHookOutput && (
		!hookOutput || typeof hookOutput !== "object" || Array.isArray(hookOutput) ||
		hookOutput.hookEventName !== "PreToolUse" ||
		hookOutput.permissionDecision !== "allow"
	);
	if (continuationStopped || decisionStopped || policyStopped || hookStopped) {
		return { action: "block", reason: guardReason(payload) };
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

// JSON.parse uses the last value for repeated keys. Reject duplicates before
// parsing so a later "allow" cannot replace an earlier stop, even when an
// escaped key or a nested classification field is involved.
function hasDuplicateJsonObjectKeys(source) {
	const frames = [];
	for (let i = 0; i < source.length;) {
		const ch = source[i];
		if (ch === '"') {
			const start = i++;
			while (i < source.length) {
				if (source[i] === "\\") {
					i += 2;
					continue;
				}
				if (source[i++] === '"') break;
			}
			const frame = frames[frames.length - 1];
			if (frame?.isObject && frame.expectKey) {
				const key = JSON.parse(source.slice(start, i));
				if (frame.keys.has(key)) return true;
				frame.keys.add(key);
				frame.expectKey = false;
			}
			continue;
		}
		if (ch === "{") frames.push({ isObject: true, expectKey: true, keys: new Set() });
		else if (ch === "[") frames.push({ isObject: false });
		else if (ch === "}" || ch === "]") frames.pop();
		else if (ch === "," && frames[frames.length - 1]?.isObject) {
			frames[frames.length - 1].expectKey = true;
		}
		i++;
	}
	return false;
}

export function guardJsonObject(stdout) {
	const output = typeof stdout === "string" ? stdout.trim() : "";
	if (!output) return null;
	try {
		if (hasDuplicateJsonObjectKeys(output)) return null;
		const value = JSON.parse(output);
		return value && typeof value === "object" && !Array.isArray(value) ? value : null;
	} catch {
		// Mixed output (including diagnostics) is not one authoritative verdict.
		return null;
	}
}

function guardArgs(command) {
	return ["command", "test", "--json", "--", command];
}

export async function evaluateWithGuard(ctx, config = {}) {
	// A missing or malformed tool identity is not evidence of an unrelated tool.
	// Host hooks can otherwise fail open before Guard sees the pending cli action.
	const tool = ctx?.tool;
	if (typeof tool !== "string" || !tool.trim()) {
		return { action: "block", reason: "HOL Guard could not identify the tool being called." };
	}
	if (tool !== "cli") return { action: "allow" };
	// Inspect the exact command GitAgent will execute; trimming can change shell/heredoc input.
	const originalArgs = ctx?.args;
	const command = originalArgs?.command;
	const originalTimeout = originalArgs?.timeout;
	if (typeof command !== "string" || !command.trim()) {
		return { action: "block", reason: "HOL Guard could not inspect a cli command." };
	}

	const configuredBinary = nonEmptyString(config.binary) || nonEmptyString(process.env.HOL_GUARD_BIN) || "hol-guard";
	const configuredTimeout = Number(config.timeout_ms);
	const timeoutMs = Number.isFinite(configuredTimeout) && configuredTimeout > 0
		? configuredTimeout
		: DEFAULT_TIMEOUT_MS;
	const workspace = nonEmptyString(config.workspace);
	// The builtin cli tool executes in the agent directory, which need not be
	// process.cwd(). Fail closed rather than inspect a different checkout.
	if (!workspace || !isAbsolute(workspace)) {
		return { action: "block", reason: "HOL Guard needs an absolute workspace matching the agent cli directory." };
	}
	const childEnv = { ...process.env };
	// Node resolves relative PATH entries after changing to the spawn cwd.
	// Bind them to this launcher's directory so a workspace-local binary
	// cannot shadow HOL Guard during command inspection.
	const launchDirectory = process.cwd();
	// A configured relative executable path must not be interpreted inside the
	// untrusted CLI workspace after spawn changes cwd. Bare names still use PATH.
	const binary = configuredBinary.includes("/") || configuredBinary.includes("\\")
		? resolve(launchDirectory, configuredBinary)
		: configuredBinary;
	let realWorkspace;
	let realLaunchWorktree;
	try {
		const initialWorkspace = realpathSync(workspace);
		// GitAgent can execute from a nested directory. PATH entries in the
		// parent Git checkout are just as untrusted as those under cwd.
		realWorkspace = initialWorkspace;
		let cursor = initialWorkspace;
		while (true) {
			if (existsSync(join(cursor, ".git"))) realWorkspace = cursor;
			const parent = dirname(cursor);
			if (parent === cursor) break;
			cursor = parent;
		}
		// A different launch checkout can also put forged binaries on PATH.
		cursor = realpathSync(launchDirectory);
		realLaunchWorktree = cursor;
		while (true) {
			if (existsSync(join(cursor, ".git"))) realLaunchWorktree = cursor;
			const parent = dirname(cursor);
			if (parent === cursor) break;
			cursor = parent;
		}
	} catch {
		return { action: "block", reason: "HOL Guard could not verify the cli workspace." };
	}
	// Explicit binary paths bypass PATH sanitization. Reject executables in
	// the untrusted CLI worktree, including a worktree-controlled symlink
	// pointing to a trusted-looking external executable.
	if (binary.includes("/") || binary.includes("\\")) {
		let realBinary;
		try {
			realBinary = realpathSync(binary);
		} catch {
			return { action: "block", reason: "HOL Guard executable path could not be verified." };
		}
		const withinWorktree = (candidate) => [realWorkspace, realLaunchWorktree].some((root) => {
			const rel = relative(root, candidate);
			return rel === "" || (rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel));
		});
		if (withinWorktree(binary) || withinWorktree(realBinary)) {
			return { action: "block", reason: "HOL Guard executable is controlled by the cli worktree." };
		}
	}
	for (const key of Object.keys(childEnv)) {
		if (key.toUpperCase() !== "PATH" || typeof childEnv[key] !== "string") continue;
		childEnv[key] = childEnv[key]
			.split(delimiter)
			.map((entry) => isAbsolute(entry) ? entry : resolve(launchDirectory, entry || "."))
			.map((entry) => {
				// Check both the selected PATH alias and its resolved directory.
				// A project-owned symlink can otherwise point outside the checkout
				// until an attacker swaps it for a forged executable at spawn time.
				let realEntry;
				try {
					realEntry = realpathSync(entry);
				} catch {
					return null;
				}
				const outsideWorkspace = (candidate) => [realWorkspace, realLaunchWorktree].every((root) => {
					const within = relative(root, candidate);
					return within !== "" && (
						within === ".." || within.startsWith(`..${sep}`) || isAbsolute(within)
					);
				});
				if (!outsideWorkspace(entry) || !outsideWorkspace(realEntry)) return null;
				// A globally located PATH directory can still contain a symlinked
				// hol-guard executable pointing into an untrusted checkout.
				for (const executableName of process.platform === "win32"
					? ["hol-guard.exe", "hol-guard.cmd", "hol-guard.bat", "hol-guard"]
					: ["hol-guard"]) {
					const executable = join(realEntry, executableName);
					try {
						const target = realpathSync(executable);
						if (!outsideWorkspace(executable) || !outsideWorkspace(target)) return null;
					} catch (error) {
						if (error?.code !== "ENOENT") return null;
					}
				}
				return realEntry;
			})
			.filter((entry) => entry !== null)
			.join(delimiter);
	}
	// An empty PATH is not a safe failure mode: on POSIX, spawn with PATH=""
	// searches cwd. After removing worktree-controlled PATH entries, a bare
	// executable name could otherwise launch ./hol-guard from the untrusted repo.
	if (!binary.includes("/") && !binary.includes("\\") &&
		!Object.keys(childEnv).some((key) => key.toUpperCase() === "PATH" && childEnv[key])) {
		return { action: "block", reason: "HOL Guard executable PATH contains no trusted directory." };
	}
	const guardHome = nonEmptyString(config.guard_home);
	if (guardHome) childEnv.HOL_GUARD_HOME = guardHome;
	const home = nonEmptyString(config.home);
	if (home) childEnv.HOME = home;

	const verdict = await new Promise((resolve) => {
		const stdoutChunks = [];
		const stderrChunks = [];
		let outputBytes = 0;
		let settled = false;
		let child;
		try {
			child = spawn(binary, guardArgs(command), {
				stdio: ["ignore", "pipe", "pipe"],
				env: childEnv,
				cwd: workspace,
				shell: false,
			});
		} catch {
			// Node can throw synchronously for invalid argv (e.g. embedded NUL).
			// Reject the hook rather than propagating a rejected promise to GitAgent.
			resolve({ action: "block", reason: "HOL Guard could not start due to invalid process arguments." });
			return;
		}

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

		const captureOutput = (chunk, stream) => {
			if (settled) return;
			outputBytes += chunk.length;
			if (outputBytes > MAX_GUARD_OUTPUT_BYTES) {
				child.kill("SIGTERM");
				finish({ action: "block", reason: "HOL Guard output exceeded the maximum size." });
				return;
			}
			// Decode only after all chunks arrive. Per-chunk UTF-8 conversion
			// could silently replace malformed bytes in an allow-shaped verdict.
			if (stream === "stdout") stdoutChunks.push(chunk);
			else stderrChunks.push(chunk);
		};
		child.stdout.on("data", (chunk) => captureOutput(chunk, "stdout"));
		child.stderr.on("data", (chunk) => captureOutput(chunk, "stderr"));
		child.on("error", (error) => {
			finish({ action: "block", reason: `HOL Guard could not start: ${error.message}` });
		});
		child.on("close", (code) => {
			if (settled) return;
			if (code !== 0) {
				finish({
					action: "block",
					reason: nonEmptyString(Buffer.concat(stderrChunks).toString("utf-8")) || `HOL Guard exited with code ${code}.`,
				});
				return;
			}
			let stdout;
			try {
				stdout = new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(stdoutChunks));
			} catch {
				finish({ action: "block", reason: "HOL Guard output was not valid UTF-8." });
				return;
			}
			finish(guardResponseToHookResult(guardJsonObject(stdout)));
		});
	});
	// GitAgent passes the hook the same args object its CLI will execute. If
	// another async callback replaces it while Guard is inspecting, an old
	// benign verdict must not authorize the substituted tool or command.
	if (ctx?.tool !== "cli" || ctx?.args !== originalArgs ||
		ctx?.args?.command !== command || ctx?.args?.timeout !== originalTimeout) {
		return { action: "block", reason: "HOL Guard tool input changed during inspection." };
	}
	return verdict;
}

export async function register(api) {
	api.registerHook("pre_tool_use", async (ctx) => {
		try {
			return await evaluateWithGuard(ctx, api.config);
		} catch {
			// GitAgent's hook runner logs unexpected exceptions and otherwise allows the
			// tool. Never let an internal adapter failure bypass this security gate.
			return { action: "block", reason: "HOL Guard could not complete the pre-tool check." };
		}
	});
}
