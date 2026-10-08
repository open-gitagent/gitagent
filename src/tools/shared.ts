import { Type } from "@sinclair/typebox";
import { posix } from "path";

// ── Constants ───────────────────────────────────────────────────────────

export const MAX_OUTPUT = 100_000; // ~100KB max output to send to LLM
export const MAX_LINES = 2000;
export const MAX_BYTES = 100_000;
export const DEFAULT_TIMEOUT = 120;
export const DEFAULT_MEMORY_PATH = "memory/MEMORY.md";

// ── Schemas ─────────────────────────────────────────────────────────────

export const cliSchema = Type.Object({
	command: Type.String({ description: "Shell command to execute" }),
	timeout: Type.Optional(Type.Number({ description: "Timeout in seconds (default: 120)" })),
});

export const readSchema = Type.Object({
	path: Type.String({ description: "Path to the file to read (relative or absolute)" }),
	offset: Type.Optional(Type.Number({ description: "Line number to start from (1-indexed)" })),
	limit: Type.Optional(Type.Number({ description: "Maximum number of lines to read" })),
});

export const writeSchema = Type.Object({
	path: Type.String({ description: "Path to the file to write (relative or absolute)" }),
	content: Type.String({ description: "Content to write to the file" }),
	createDirs: Type.Optional(Type.Boolean({ description: "Create parent directories if needed (default: true)" })),
});

export const editSchema = Type.Object({
	path: Type.String({ description: "Path to the file to edit (relative or absolute)" }),
	old_string: Type.String({ description: "Exact text to find and replace. Must match uniquely unless replace_all is true." }),
	new_string: Type.String({ description: "Replacement text" }),
	replace_all: Type.Optional(Type.Boolean({ description: "Replace every occurrence (default: false)" })),
	regex: Type.Optional(Type.Boolean({ description: "Treat old_string as a JavaScript regular expression (default: false). When true, new_string may reference groups like $1." })),
	flags: Type.Optional(Type.String({ description: "Regex flags (e.g. 'i', 'm', 's'). Only used when regex=true. 'g' is added automatically when replace_all is true." })),
});

export const memorySchema = Type.Object({
	action: Type.Union([Type.Literal("load"), Type.Literal("save")], { description: "Whether to load or save memory" }),
	content: Type.Optional(Type.String({ description: "Memory content to save (required for save)" })),
	message: Type.Optional(Type.String({ description: "Commit message describing why this memory changed (required for save)" })),
});

export const taskTrackerSchema = Type.Object({
	action: Type.Union([Type.Literal("begin"), Type.Literal("update"), Type.Literal("end"), Type.Literal("list")], { description: "Action to perform" }),
	objective: Type.Optional(Type.String({ description: "Task objective (required for begin)" })),
	task_id: Type.Optional(Type.String({ description: "Task ID (required for update/end)" })),
	step: Type.Optional(Type.String({ description: "Step description (for update)" })),
	outcome: Type.Optional(Type.Union([Type.Literal("success"), Type.Literal("failure"), Type.Literal("partial")], { description: "Task outcome (for end)" })),
	failure_reason: Type.Optional(Type.String({ description: "Why the task failed (for end+failure)" })),
	skill_used: Type.Optional(Type.String({ description: "Name of skill used, if any (for end)" })),
});

export const capturePhotoSchema = Type.Object({
	reason: Type.String({ description: "Why this moment is being captured (e.g. 'user celebrating project launch')" }),
});

export const skillLearnerSchema = Type.Object({
	action: Type.Union([Type.Literal("evaluate"), Type.Literal("crystallize"), Type.Literal("status"), Type.Literal("review"), Type.Literal("update"), Type.Literal("delete")], { description: "Action to perform" }),
	task_id: Type.Optional(Type.String({ description: "Task ID (for evaluate/crystallize)" })),
	skill_name: Type.Optional(Type.String({ description: "Skill name (for crystallize/update/delete)" })),
	skill_description: Type.Optional(Type.String({ description: "Skill description (for crystallize)" })),
	instructions: Type.Optional(Type.String({ description: "New instructions content (for update)" })),
	override_heuristic: Type.Optional(Type.Boolean({ description: "Override skill-worthiness heuristic (for evaluate)" })),
});

// ── Shared helpers ──────────────────────────────────────────────────────

/** Truncate output to MAX_OUTPUT, keeping the tail. */
export function truncateOutput(text: string): string {
	if (text.length > MAX_OUTPUT) {
		return `[output truncated, showing last ~100KB]\n${text.slice(-MAX_OUTPUT)}`;
	}
	return text;
}

/**
 * Paginate text by lines with offset (1-indexed) and limit.
 * Returns { text, hasMore, shownRange, totalLines }.
 */
export function paginateLines(
	text: string,
	offset?: number,
	limit?: number,
): { text: string; hasMore: boolean; shownRange: [number, number]; totalLines: number } {
	const allLines = text.split("\n");
	const totalLines = allLines.length;

	const startLine = offset ? Math.max(0, offset - 1) : 0;
	if (startLine >= totalLines) {
		throw new Error(`Offset ${offset} is beyond end of file (${totalLines} lines)`);
	}

	const maxLines = limit ?? MAX_LINES;
	const lastLine = Math.min(startLine + maxLines, totalLines);

	// Take whole lines until the UTF-8 byte budget is spent, so the reported
	// range and the next offset match exactly what was shown.
	let bytes = 0;
	let endLine = startLine;
	for (let i = startLine; i < lastLine; i++) {
		const lineBytes = Buffer.byteLength(allLines[i], "utf-8") + (i > startLine ? 1 : 0);
		if (bytes + lineBytes > MAX_BYTES) break;
		bytes += lineBytes;
		endLine = i + 1;
	}

	let selected: string;
	if (endLine === startLine) {
		// A single line larger than the budget: show its head, cut on a
		// character boundary, and move on to the next line.
		const buf = Buffer.from(allLines[startLine], "utf-8");
		let cut = MAX_BYTES;
		while (cut > 0 && (buf[cut] & 0xc0) === 0x80) cut--; // don't split a multi-byte char
		const head = buf.subarray(0, cut).toString("utf-8");
		selected = `${head}\n[line ${startLine + 1} truncated: longer than ${MAX_BYTES} bytes]`;
		endLine = startLine + 1;
	} else {
		selected = allLines.slice(startLine, endLine).join("\n");
	}

	const hasMore = endLine < totalLines;

	return {
		text: selected,
		hasMore,
		shownRange: [startLine + 1, endLine],
		totalLines,
	};
}

/**
 * Resolve a path relative to a sandbox repo root. `~` refers to the sandbox
 * user's home (the repo root's parent, e.g. /home/user), never the host's.
 */
export function resolveSandboxPath(path: string, repoRoot: string, sandboxHome?: string): string {
	if (path.startsWith("~/") || path === "~") {
		const home = sandboxHome ?? posix.dirname(repoRoot.replace(/\/+$/, ""));
		path = home + path.slice(1);
	}
	if (path.startsWith("/")) return path;
	return repoRoot.endsWith("/") ? repoRoot + path : repoRoot + "/" + path;
}
