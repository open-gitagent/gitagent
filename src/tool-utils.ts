import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { GCToolDefinition } from "./sdk-types.js";
import { buildTypeboxSchema } from "./tool-loader.js";

// ── Convert GCToolDefinition → AgentTool ───────────────────────────────

export function toAgentTool(def: GCToolDefinition): AgentTool<any> {
	const schema = buildTypeboxSchema(def.inputSchema);

	return {
		name: def.name,
		label: def.name,
		description: def.description,
		parameters: schema,
		execute: async (
			_toolCallId: string,
			params: any,
			signal?: AbortSignal,
		) => {
			const result = await def.handler(params, signal);
			const text = typeof result === "string" ? result : result.text;
			const details = typeof result === "object" && "details" in result
				? result.details
				: undefined;
			return { content: [{ type: "text" as const, text }], details };
		},
	};
}
