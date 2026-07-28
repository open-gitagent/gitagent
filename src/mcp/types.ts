import type { AgentTool } from "@mariozechner/pi-agent-core";

/** A local MCP server launched as a child process, spoken to over stdio. */
export interface McpStdioServerConfig {
	type?: "stdio";
	command: string;
	args?: string[];
	env?: Record<string, string>;
	cwd?: string;
	/** Connect + initial listTools timeout in ms. Default 30000. */
	timeoutMs?: number;
}

/** A remote MCP server reached over Streamable HTTP (or legacy SSE). */
export interface McpHttpServerConfig {
	type: "http" | "sse";
	url: string;
	headers?: Record<string, string>;
	/** Connect + initial listTools timeout in ms. Default 30000. */
	timeoutMs?: number;
}

export type McpServerConfig = McpStdioServerConfig | McpHttpServerConfig;

/** Result of wiring up all configured MCP servers for a session. */
export interface McpSetupResult {
	/** Tools discovered across all servers, namespaced `<server>__<tool>`. */
	tools: AgentTool<any>[];
	/** Idempotent teardown — closes every transport/client. Safe to call repeatedly. */
	cleanup: () => Promise<void>;
}
