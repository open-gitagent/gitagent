import { mkdir, writeFile } from "fs/promises";
import { dirname } from "path";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { writeSchema, resolveJailed } from "./shared.js";

export function createWriteTool(cwd: string, rootDir?: string): AgentTool<typeof writeSchema> {
	return {
		name: "write",
		label: "write",
		description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Parent directories are created automatically.",
		parameters: writeSchema,
		execute: async (
			_toolCallId: string,
			{ path, content, createDirs }: { path: string; content: string; createDirs?: boolean },
			signal?: AbortSignal,
		) => {
			if (signal?.aborted) throw new Error("Operation aborted");

			const absolutePath = resolveJailed(path, cwd, rootDir);

			if (createDirs !== false) {
				await mkdir(dirname(absolutePath), { recursive: true });
			}

			await writeFile(absolutePath, content, "utf-8");

			const bytes = Buffer.byteLength(content, "utf-8");
			return {
				content: [{ type: "text", text: `Wrote ${bytes} bytes to ${path}` }],
				details: undefined,
			};
		},
	};
}
