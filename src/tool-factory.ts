import type { AgentTool, AgentToolUpdateCallback } from "@mariozechner/pi-agent-core";
import { buildTypeboxSchema } from "./tool-loader.js";

// ── Tool metadata for concurrency, safety, and budget ─────────────────

export interface ToolMetadata {
	/** Can run in parallel with other concurrent-safe tools. Default: false (fail-closed) */
	isConcurrencySafe?: boolean;
	/** Only reads, never writes. Default: false (fail-closed) */
	isReadOnly?: boolean;
	/** Irreversible action (delete, send). Default: false */
	isDestructive?: boolean;
	/** Truncate result if larger than this. Default: 50000 chars */
	maxResultSizeChars?: number;
}

export interface ToolDefinition<T = any> {
	name: string;
	description: string;
	parameters: Record<string, any>;
	execute: (args: T, signal?: AbortSignal) => Promise<string>;
	metadata?: ToolMetadata;
}

const TOOL_DEFAULTS: Required<ToolMetadata> = {
	isConcurrencySafe: false,
	isReadOnly: false,
	isDestructive: false,
	maxResultSizeChars: 50000,
};

/**
 * Build a tool with fail-closed defaults and result truncation.
 * Mirrors Claude Code's buildTool() pattern.
 */
export function buildTool<T = any>(def: ToolDefinition<T>): AgentTool<any> & { metadata: Required<ToolMetadata> } {
	const metadata: Required<ToolMetadata> = { ...TOOL_DEFAULTS, ...def.metadata };
	const schema = buildTypeboxSchema(def.parameters);

	return {
		name: def.name,
		label: def.name,
		description: def.description,
		parameters: schema,
		metadata,
		async execute(
			toolCallId: string,
			params: unknown,
			signal?: AbortSignal,
			_onUpdate?: AgentToolUpdateCallback,
		) {
			let result = await def.execute(params as T, signal);
			if (result.length > metadata.maxResultSizeChars) {
				result = result.slice(0, metadata.maxResultSizeChars) +
					`\n\n[Truncated: ${result.length} chars total, showing first ${metadata.maxResultSizeChars}]`;
			}
			return { content: [{ type: "text" as const, text: result }], details: undefined };
		},
	};
}

/**
 * Get metadata for a tool, returning fail-closed defaults if not set.
 */
export function getToolMetadata(tool: AgentTool<any>): Required<ToolMetadata> {
	return (tool as any).metadata ?? { ...TOOL_DEFAULTS };
}
