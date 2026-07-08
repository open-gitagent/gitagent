import { readFile } from "fs/promises";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { readSchema, MAX_LINES, paginateLines, resolveJailed } from "./shared.js";

function isBinary(buffer: Buffer): boolean {
	// Check first 8KB for null bytes
	const check = buffer.subarray(0, 8192);
	for (let i = 0; i < check.length; i++) {
		if (check[i] === 0) return true;
	}
	return false;
}

export function createReadTool(cwd: string, rootDir?: string): AgentTool<typeof readSchema> {
	return {
		name: "read",
		label: "read",
		description: `Read the contents of a file. Output is limited to ${MAX_LINES} lines or ~100KB. Use offset/limit for large files.`,
		parameters: readSchema,
		execute: async (
			_toolCallId: string,
			{ path, offset, limit }: { path: string; offset?: number; limit?: number },
			signal?: AbortSignal,
		) => {
			if (signal?.aborted) throw new Error("Operation aborted");

			const absolutePath = resolveJailed(path, cwd, rootDir);
			const buffer = await readFile(absolutePath);

			if (isBinary(buffer)) {
				return {
					content: [{ type: "text", text: `[Binary file: ${path} (${buffer.length} bytes)]` }],
					details: undefined,
				};
			}

			const text = buffer.toString("utf-8");
			const page = paginateLines(text, offset, limit);
			let result = page.text;

			if (page.hasMore) {
				const nextOffset = page.shownRange[1] + 1;
				result += `\n\n[Showing lines ${page.shownRange[0]}-${page.shownRange[1]} of ${page.totalLines}. Use offset=${nextOffset} to continue.]`;
			}

			return {
				content: [{ type: "text", text: result }],
				details: undefined,
			};
		},
	};
}
