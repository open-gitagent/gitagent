import { type Static } from "@sinclair/typebox";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { SandboxContext } from "../sandbox.js";
import { memorySchema, DEFAULT_MEMORY_PATH, resolveSandboxPath } from "./shared.js";
import yaml from "js-yaml";

interface MemoryLayer {
	name: string;
	path: string;
	max_lines?: number;
	format: "markdown" | "yaml";
}

interface MemoryConfig {
	layers: MemoryLayer[];
	archive_policy?: { max_entries?: number; compress_after?: string };
}

async function loadMemoryConfig(ctx: SandboxContext): Promise<MemoryConfig | null> {
	try {
		const raw: string = await ctx.machine.readFile(
			resolveSandboxPath("memory/memory.yaml", ctx.repoPath),
		);
		const config = yaml.load(raw) as MemoryConfig;
		if (!config?.layers || !Array.isArray(config.layers)) return null;
		return config;
	} catch {
		return null;
	}
}

function getWorkingLayer(config: MemoryConfig | null): { path: string; maxLines?: number } {
	if (!config) return { path: DEFAULT_MEMORY_PATH };
	const working = config.layers.find((l) => l.name === "working") || config.layers[0];
	if (!working) return { path: DEFAULT_MEMORY_PATH };
	return { path: working.path, maxLines: working.max_lines };
}

async function archiveOverflow(
	ctx: SandboxContext,
	content: string,
	maxLines: number,
): Promise<string> {
	const lines = content.split("\n");
	if (lines.length <= maxLines) return content;

	const overflow = lines.slice(0, lines.length - maxLines).join("\n");
	const kept = lines.slice(lines.length - maxLines).join("\n");

	const now = new Date();
	const archiveFile = `memory/archive/${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}.md`;
	const archivePath = resolveSandboxPath(archiveFile, ctx.repoPath);

	// Ensure archive directory exists
	await ctx.gitMachine.run(`mkdir -p "${archivePath.substring(0, archivePath.lastIndexOf("/"))}"`, {
		cwd: ctx.repoPath,
	});

	// Append to archive
	let existing = "";
	try {
		existing = await ctx.machine.readFile(archivePath);
	} catch {
		// New archive file
	}

	const archiveEntry = `\n---\n_Archived: ${now.toISOString()}_\n\n${overflow}\n`;
	await ctx.machine.writeFile(archivePath, existing + archiveEntry);

	return kept;
}

export function createSandboxMemoryTool(ctx: SandboxContext): AgentTool<typeof memorySchema> {
	return {
		name: "memory",
		label: "memory",
		description:
			"Git-backed memory in the sandbox VM. Use 'load' to read current memory, 'save' to update memory and commit to git. Each save creates a git commit, giving you full history.",
		parameters: memorySchema,
		execute: async (
			_toolCallId: string,
			rawParams: unknown,
			signal?: AbortSignal,
		) => {
			const { action, content, message } = rawParams as Static<typeof memorySchema>;
			if (signal?.aborted) throw new Error("Operation aborted");

			const config = await loadMemoryConfig(ctx);
			const { path: memoryPath, maxLines } = getWorkingLayer(config);
			const memoryFile = resolveSandboxPath(memoryPath, ctx.repoPath);

			if (action === "load") {
				try {
					const text: string = await ctx.machine.readFile(memoryFile);
					const trimmed = text.trim();
					if (!trimmed || trimmed === "# Memory") {
						return {
							content: [{ type: "text", text: "No memories yet." }],
							details: undefined,
						};
					}
					return {
						content: [{ type: "text", text: trimmed }],
						details: undefined,
					};
				} catch {
					return {
						content: [{ type: "text", text: "No memories yet." }],
						details: undefined,
					};
				}
			}

			// action === "save"
			if (!content) {
				throw new Error("content is required for save action");
			}

			const commitMsg = message || "Update memory";

			let finalContent = content;
			if (maxLines) {
				finalContent = await archiveOverflow(ctx, content, maxLines);
			}

			// Ensure parent directory exists
			const dir = memoryFile.substring(0, memoryFile.lastIndexOf("/"));
			if (dir) {
				await ctx.gitMachine.run(`mkdir -p "${dir}"`, { cwd: ctx.repoPath });
			}

			await ctx.machine.writeFile(memoryFile, finalContent);

			try {
				await ctx.gitMachine.commit(commitMsg);
			} catch (err: any) {
				return {
					content: [
						{
							type: "text",
							text: `Memory saved to ${memoryPath} but git commit failed: ${err.message || "unknown error"}. The file was still written.`,
						},
					],
					details: undefined,
				};
			}

			return {
				content: [{ type: "text", text: `Memory saved and committed: "${commitMsg}"` }],
				details: undefined,
			};
		},
	};
}
