import { readFile, writeFile, mkdir } from "fs/promises";
import { join, dirname } from "path";
import { execSync } from "child_process";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { memorySchema, DEFAULT_MEMORY_PATH } from "./shared.js";
import yaml from "js-yaml";
import type { MemoryLayerDef } from "../plugin-types.js";

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

async function loadMemoryConfig(cwd: string, pluginLayers?: MemoryLayerDef[]): Promise<MemoryConfig | null> {
	let config: MemoryConfig | null = null;
	try {
		const raw = await readFile(join(cwd, "memory", "memory.yaml"), "utf-8");
		const parsed = yaml.load(raw) as MemoryConfig;
		if (parsed?.layers && Array.isArray(parsed.layers)) {
			config = parsed;
		}
	} catch {
		// No config file
	}

	// Merge plugin memory layers
	if (pluginLayers && pluginLayers.length > 0) {
		if (!config) config = { layers: [] };
		for (const layer of pluginLayers) {
			config.layers.push({
				name: layer.name,
				path: layer.path,
				format: "markdown",
			});
		}
	}

	return config;
}

function getWorkingLayer(config: MemoryConfig | null): { path: string; maxLines?: number } {
	if (!config) {
		return { path: DEFAULT_MEMORY_PATH };
	}
	const working = config.layers.find((l) => l.name === "working") || config.layers[0];
	if (!working) {
		return { path: DEFAULT_MEMORY_PATH };
	}
	return { path: working.path, maxLines: working.max_lines };
}

async function archiveOverflow(
	cwd: string,
	content: string,
	maxLines: number,
): Promise<string> {
	const lines = content.split("\n");
	if (lines.length <= maxLines) return content;

	// Keep the last maxLines, archive the rest
	const overflow = lines.slice(0, lines.length - maxLines).join("\n");
	const kept = lines.slice(lines.length - maxLines).join("\n");

	const now = new Date();
	const archiveFile = `memory/archive/${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}.md`;
	const archivePath = join(cwd, archiveFile);

	await mkdir(dirname(archivePath), { recursive: true });

	// Append to archive
	let existing = "";
	try {
		existing = await readFile(archivePath, "utf-8");
	} catch {
		// New archive file
	}

	const archiveEntry = `\n---\n_Archived: ${now.toISOString()}_\n\n${overflow}\n`;
	await writeFile(archivePath, existing + archiveEntry, "utf-8");

	// Try to git add the archive
	try {
		execSync(`git add "${archiveFile}"`, { cwd, stdio: "pipe" });
	} catch {
		// Not in git, that's fine
	}

	return kept;
}

export function createMemoryTool(cwd: string, pluginLayers?: MemoryLayerDef[]): AgentTool<typeof memorySchema> {
	return {
		name: "memory",
		label: "memory",
		description:
			"Git-backed memory. Use 'load' to read current memory, 'save' to update memory and commit to git. Each save creates a git commit, giving you full history of what you've remembered.",
		parameters: memorySchema,
		execute: async (
			_toolCallId: string,
			{ action, content, message }: { action: string; content?: string; message?: string },
			signal?: AbortSignal,
		) => {
			if (signal?.aborted) throw new Error("Operation aborted");

			const config = await loadMemoryConfig(cwd, pluginLayers);
			const { path: memoryPath, maxLines } = getWorkingLayer(config);
			const memoryFile = join(cwd, memoryPath);

			if (action === "load") {
				try {
					const text = await readFile(memoryFile, "utf-8");
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

			// Apply max_lines archiving if configured
			let finalContent = content;
			if (maxLines) {
				finalContent = await archiveOverflow(cwd, content, maxLines);
			}

			await mkdir(dirname(memoryFile), { recursive: true });
			await writeFile(memoryFile, finalContent, "utf-8");

			try {
				execSync(`git add "${memoryPath}" && git commit -m "${commitMsg.replace(/"/g, '\\"')}"`, {
					cwd,
					stdio: "pipe",
				});
			} catch (err: any) {
				const stderr = err.stderr?.toString() || "";
				return {
					content: [
						{
							type: "text",
							text: `Memory saved to ${memoryPath} but git commit failed: ${stderr.trim() || "unknown error"}. The file was still written.`,
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
