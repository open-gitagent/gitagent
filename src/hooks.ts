import { spawn } from "child_process";
import { readFile } from "fs/promises";
import { join, resolve } from "path";
import yaml from "js-yaml";
import type { AgentTool } from "@mariozechner/pi-agent-core";

export interface HookDefinition {
	script: string;
	description?: string;
	baseDir?: string; // plugin hooks run from their own directory
	_handler?: (ctx: Record<string, any>) => Promise<HookResult> | HookResult;
}

export interface HooksConfig {
	hooks: {
		on_session_start?: HookDefinition[];
		pre_tool_use?: HookDefinition[];
		post_response?: HookDefinition[];
		on_error?: HookDefinition[];
	};
}

export interface HookResult {
	action: "allow" | "block" | "modify";
	reason?: string;
	args?: Record<string, any>;
}

export async function loadHooksConfig(agentDir: string): Promise<HooksConfig | null> {
	const hooksPath = join(agentDir, "hooks", "hooks.yaml");
	try {
		const raw = await readFile(hooksPath, "utf-8");
		const config = yaml.load(raw) as HooksConfig;
		if (!config?.hooks) return null;
		return config;
	} catch {
		return null;
	}
}

async function executeHook(
	hook: HookDefinition,
	agentDir: string,
	input: Record<string, any>,
): Promise<HookResult> {
	// Programmatic hooks: call handler directly instead of spawning shell
	if (typeof hook._handler === "function") {
		try {
			const result = await hook._handler(input);
			return result ?? { action: "allow" };
		} catch (err: any) {
			throw new Error(`Programmatic hook "${hook.description || hook.script}" failed: ${err.message}`);
		}
	}

	return new Promise((promiseResolve, reject) => {
		// Plugin hooks use baseDir; agent hooks resolve relative to hooks/ dir
		const baseDir = hook.baseDir || agentDir;
		const scriptPath = hook.baseDir
			? join(baseDir, hook.script)
			: join(agentDir, "hooks", hook.script);

		// Path traversal guard: ensure script doesn't escape its base directory
		const resolvedScript = resolve(scriptPath);
		const allowedBase = resolve(baseDir);
		if (!resolvedScript.startsWith(allowedBase + "/") && resolvedScript !== allowedBase) {
			reject(new Error(`Hook "${hook.script}" escapes its base directory`));
			return;
		}

		const child = spawn("sh", [resolvedScript], {
			cwd: baseDir,
			stdio: ["pipe", "pipe", "pipe"],
			env: { ...process.env },
		});

		let stdout = "";
		let stderr = "";

		child.stdout.on("data", (data: Buffer) => {
			stdout += data.toString("utf-8");
		});
		child.stderr.on("data", (data: Buffer) => {
			stderr += data.toString("utf-8");
		});

		child.stdin.write(JSON.stringify(input));
		child.stdin.end();

		const timeout = setTimeout(() => {
			child.kill("SIGTERM");
			reject(new Error(`Hook "${hook.script}" timed out after 10s`));
		}, 10_000);

		child.on("error", (err) => {
			clearTimeout(timeout);
			reject(new Error(`Hook "${hook.script}" failed to start: ${err.message}`));
		});

		child.on("close", (code) => {
			clearTimeout(timeout);
			if (code !== 0) {
				reject(new Error(`Hook "${hook.script}" exited with code ${code}: ${stderr.trim()}`));
				return;
			}
			try {
				const result = JSON.parse(stdout.trim()) as HookResult;
				promiseResolve(result);
			} catch {
				// If hook doesn't return JSON, treat as allow
				promiseResolve({ action: "allow" });
			}
		});
	});
}

export async function runHooks(
	hooks: HookDefinition[] | undefined,
	agentDir: string,
	input: Record<string, any>,
): Promise<HookResult> {
	if (!hooks || hooks.length === 0) {
		return { action: "allow" };
	}

	for (const hook of hooks) {
		try {
			const result = await executeHook(hook, agentDir, input);
			if (result.action === "block") {
				return result;
			}
			if (result.action === "modify") {
				return result;
			}
		} catch (err: any) {
			console.error(`Hook error: ${err.message}`);
			// Hook errors don't block execution by default
		}
	}

	return { action: "allow" };
}

/**
 * Wraps a tool's execute function with pre_tool_use hook support.
 */
export function wrapToolWithHooks<T extends AgentTool<any>>(
	tool: T,
	hooksConfig: HooksConfig,
	agentDir: string,
	sessionId: string,
): T {
	const preToolHooks = hooksConfig.hooks.pre_tool_use;
	if (!preToolHooks || preToolHooks.length === 0) return tool;

	const originalExecute = tool.execute;

	const wrappedTool = {
		...tool,
		execute: async (
			toolCallId: string,
			args: any,
			signal?: AbortSignal,
			onUpdate?: any,
		) => {
			const hookInput = {
				event: "pre_tool_use",
				session_id: sessionId,
				tool: tool.name,
				args,
			};

			const result = await runHooks(preToolHooks, agentDir, hookInput);

			if (result.action === "block") {
				throw new Error(`Tool "${tool.name}" blocked by hook: ${result.reason || "no reason given"}`);
			}

			const finalArgs = result.action === "modify" && result.args ? result.args : args;
			return originalExecute.call(tool, toolCallId, finalArgs, signal, onUpdate);
		},
	};

	return wrappedTool as T;
}
