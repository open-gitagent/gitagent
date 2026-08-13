// Shared types for the lyzr-tools plugin.

export interface ResolvedConfig {
	apiKey: string;
	baseUrl: string;
	agentId?: string;
	userId?: string;
	workspaceId?: string;
	includeMcp: boolean;
	preferLyzrTools: boolean;
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
	/**
	 * Best-effort provider key inferred from the observed
	 * "<provider>-<label>" naming convention Lyzr Studio uses for a
	 * connected integration's tool_name (e.g. "gmail-Akshat Gmail
	 * Integration" -> "gmail"). Used only for auth-message wording and
	 * dedupe-prompt/local-skill matching — never sent back to Lyzr.
	 */
	provider?: string;
	/** Real Lyzr tool_source value for this tool_config ("composio" | "aci"), taken verbatim from the agent's own config. */
	toolSource?: string;
	actionName?: string;
	actionNames?: string[];
	providerUuid?: string;
	credentialId?: string;
	/**
	 * The exact tool_config entry as returned by GET /v3/agents/{agent_id}
	 * for this integration (tool_name, tool_source, action_names,
	 * persist_auth, provider_uuid, credential_id, ...). Sent back verbatim
	 * as `tool_configs[0]` on execution, since it is already the
	 * pre-validated config a human wired up in Lyzr Studio — see execute.ts.
	 */
	rawToolConfig?: Record<string, unknown>;
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
