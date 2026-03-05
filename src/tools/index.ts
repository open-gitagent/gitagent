import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { SandboxContext } from "../sandbox.js";
import { createCliTool } from "./cli.js";
import { createReadTool } from "./read.js";
import { createWriteTool } from "./write.js";
import { createMemoryTool } from "./memory.js";
import { createGitcronTool } from "./gitcron.js";
import { createSandboxCliTool } from "./sandbox-cli.js";
import { createSandboxReadTool } from "./sandbox-read.js";
import { createSandboxWriteTool } from "./sandbox-write.js";
import { createSandboxMemoryTool } from "./sandbox-memory.js";

export interface BuiltinToolsConfig {
	dir: string;
	timeout?: number;
	sandbox?: SandboxContext;
}

/**
 * Create the built-in tools (cli, read, write, memory, gitcron).
 * If a SandboxContext is provided, returns sandbox-backed tools;
 * otherwise returns the standard local tools.
 * The gitcron tool is always included — it gracefully fails if
 * gitcron CLI is not installed.
 */
export function createBuiltinTools(config: BuiltinToolsConfig): AgentTool<any>[] {
	if (config.sandbox) {
		return [
			createSandboxCliTool(config.sandbox, config.timeout),
			createSandboxReadTool(config.sandbox),
			createSandboxWriteTool(config.sandbox),
			createSandboxMemoryTool(config.sandbox),
		];
	}

	return [
		createCliTool(config.dir, config.timeout),
		createReadTool(config.dir),
		createWriteTool(config.dir),
		createMemoryTool(config.dir),
		createGitcronTool(config.dir),
	];
}
