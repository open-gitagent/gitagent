import { readFile, writeFile } from "fs/promises";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { editSchema, resolveJailed } from "./shared.js";

function countOccurrences(haystack: string, needle: string): number {
	if (!needle) return 0;
	let count = 0;
	let idx = 0;
	while ((idx = haystack.indexOf(needle, idx)) !== -1) {
		count++;
		idx += needle.length;
	}
	return count;
}

export function createEditTool(cwd: string, rootDir?: string): AgentTool<typeof editSchema> {
	return {
		name: "edit",
		label: "edit",
		description:
			"Edit a file by replacing text. By default, performs an exact string replacement that must match uniquely. Set replace_all=true to replace every occurrence. Set regex=true to treat old_string as a JS regular expression (new_string may use $1-style backreferences).",
		parameters: editSchema,
		execute: async (
			_toolCallId: string,
			{
				path,
				old_string,
				new_string,
				replace_all,
				regex,
				flags,
			}: { path: string; old_string: string; new_string: string; replace_all?: boolean; regex?: boolean; flags?: string },
			signal?: AbortSignal,
		) => {
			if (signal?.aborted) throw new Error("Operation aborted");

			const absolutePath = resolveJailed(path, cwd, rootDir);
			const original = await readFile(absolutePath, "utf-8");

			if (old_string === new_string) {
				throw new Error("old_string and new_string are identical — nothing to change");
			}

			let updated: string;
			let replacements = 0;

			if (regex) {
				let rxFlags = flags || "";
				if (replace_all && !rxFlags.includes("g")) rxFlags += "g";
				let rx: RegExp;
				try {
					rx = new RegExp(old_string, rxFlags);
				} catch (err: any) {
					throw new Error(`Invalid regex: ${err.message}`);
				}
				const matches = original.match(new RegExp(old_string, rxFlags.includes("g") ? rxFlags : rxFlags + "g"));
				replacements = matches ? matches.length : 0;
				if (replacements === 0) {
					throw new Error(`Regex pattern not found in ${path}`);
				}
				if (!replace_all && replacements > 1) {
					throw new Error(
						`Regex matched ${replacements} times in ${path}. Make the pattern more specific or set replace_all=true.`,
					);
				}
				updated = original.replace(rx, new_string);
			} else {
				if (!old_string) {
					throw new Error("old_string cannot be empty");
				}
				replacements = countOccurrences(original, old_string);
				if (replacements === 0) {
					throw new Error(`old_string not found in ${path}`);
				}
				if (!replace_all && replacements > 1) {
					throw new Error(
						`old_string matches ${replacements} times in ${path}. Provide more surrounding context to make it unique, or set replace_all=true.`,
					);
				}
				if (replace_all) {
					updated = original.split(old_string).join(new_string);
				} else {
					updated = original.replace(old_string, new_string);
				}
			}

			if (updated === original) {
				throw new Error("No changes applied — replacement produced identical content");
			}

			await writeFile(absolutePath, updated, "utf-8");

			const applied = replace_all ? replacements : 1;
			return {
				content: [{ type: "text", text: `Edited ${path} — ${applied} replacement${applied === 1 ? "" : "s"} applied` }],
				details: undefined,
			};
		},
	};
}
