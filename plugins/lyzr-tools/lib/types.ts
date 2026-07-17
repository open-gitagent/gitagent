// Shared types for the lyzr-tools plugin.

export interface ResolvedConfig {
	apiKey: string;
	baseUrl: string;
	agentId?: string;
	userId?: string;
	workspaceId?: string;
	providers: string[];
	includeMcp: boolean;
	preferLyzrTools: boolean;
	persistAuth: boolean;
	timeoutMs: number;
}

export interface ConnectedAccount {
	authorized: boolean;
	credentialId?: string;
	providerUuid?: string;
	authUrl?: string;
}

/** A tool discovered from Lyzr, normalized into a shape gitagent can register. */
export interface LyzrDiscoveredTool {
	/** The raw action/tool name as reported by Lyzr. */
	rawName: string;
	/** The gitagent-safe, prefixed tool name (e.g. "lyzr_gmail_send_email"). */
	toolName: string;
	displayName: string;
	description: string;
	inputSchema: { properties: Record<string, any>; required?: string[] };
	/** Which Lyzr execution surface this tool must be routed through. */
	execSource: "agent" | "mcp";
	/** Normalized provider key, e.g. "gmail", used for auth lookups and dedupe hints. */
	provider?: string;
	/** Provider/tool-source identifier as understood by Lyzr's ToolConfig. */
	toolSource?: string;
	actionName?: string;
	actionNames?: string[];
	providerUuid?: string;
	credentialId?: string;
	/** MCP server id, only set when execSource === "mcp". */
	serverId?: string;
	/** Whether Lyzr currently reports this provider/tool as authorized for the configured user. */
	authorized: boolean;
	authUrl?: string;
}

export interface ToolCallResult {
	text: string;
	details?: unknown;
}

export interface Logger {
	info(msg: string): void;
	warn(msg: string): void;
	error(msg: string): void;
}
