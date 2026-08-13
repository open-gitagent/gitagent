// Phase 2: Tool discovery.
//
// Discovers tools from two sources:
//
//  1. Agent tool_configs — a real, working Lyzr agent (GET
//     /v3/agents/{agent_id}) already carries a `tool_configs` array that a
//     human wired up in Lyzr Studio: one entry per connected integration,
//     each with a human-named `tool_name` (e.g.
//     "gmail-Akshat Gmail Integration", NOT the generic provider id),
//     `tool_source` ("composio" | "aci"), `action_names`
//     (UPPERCASE_SNAKE_CASE, e.g. "GMAIL_SEND_EMAIL"), and a pre-resolved
//     `provider_uuid` / `credential_id`. Reading this directly (rather than
//     independently reconstructing an equivalent config from the provider
//     catalog + connected-accounts APIs, which was tried first and got the
//     field semantics wrong — see git history) is the only source that is
//     confirmed correct against a live account, so it's used verbatim: one
//     gitagent tool per action_name, with the *entire* original tool_config
//     entry stored and sent back unchanged as `tool_configs[0]` at execution
//     time (see execute.ts).
//  2. MCP server tools — GET /v3/tools/mcp/servers + .../{server_id}/tools,
//     which *is* fully typed in Swagger (MCPServerListResponse /
//     ToolsListResponse) and unaffected by the above.
//
// Known gap: the agent's own tool_configs carry no per-action input schema
// (no field in GET /v3/agents/{agent_id} documents e.g. GMAIL_SEND_EMAIL's
// to/subject/body parameters), so tools registered here get a permissive
// empty inputSchema — the model must infer arguments from the tool's name/
// description alone. See README "Known limitations".
//
// Connected-account status (GET /v3/tools/credentials/connected_accounts)
// is still cross-referenced as a secondary signal: a non-empty
// credential_id already on the agent's tool_config is treated as the
// primary evidence of authorization (a human already connected it), OR'd
// with what connected_accounts reports, so authUrl can still be surfaced
// when available.

import type { LyzrClient } from "./client.ts";
import type { ConnectedAccount, LyzrDiscoveredTool, Logger, ResolvedConfig } from "./types.ts";
import { normalizeProviderKey, normalizeToolName } from "./normalize.ts";

export interface DiscoveryStats {
	agentToolConfigsFound: number;
	agentActionsFound: number;
	mcpServersQueried: number;
	mcpToolsFound: number;
	unauthorized: number;
	errors: string[];
}

export interface DiscoveryResult {
	tools: LyzrDiscoveredTool[];
	stats: DiscoveryStats;
}

export async function discoverLyzrTools(
	client: LyzrClient,
	config: ResolvedConfig,
	logger: Logger,
): Promise<DiscoveryResult> {
	const stats: DiscoveryStats = {
		agentToolConfigsFound: 0,
		agentActionsFound: 0,
		mcpServersQueried: 0,
		mcpToolsFound: 0,
		unauthorized: 0,
		errors: [],
	};
	const tools: LyzrDiscoveredTool[] = [];
	const seenNames = new Set<string>();

	const connected = await fetchConnectedAccounts(client, config, stats, logger);

	await discoverAgentTools(client, config, connected, tools, seenNames, stats, logger);

	if (config.includeMcp) {
		await discoverMcpTools(client, tools, seenNames, stats, logger);
	}

	return { tools, stats };
}

// ── Agent tool_configs discovery ────────────────────────────────────────

async function discoverAgentTools(
	client: LyzrClient,
	config: ResolvedConfig,
	connected: Map<string, ConnectedAccount>,
	tools: LyzrDiscoveredTool[],
	seenNames: Set<string>,
	stats: DiscoveryStats,
	logger: Logger,
): Promise<void> {
	if (!config.agentId) {
		logger.warn(
			'lyzr-tools: agent_id is not set (config "agent_id" / env "GITAGENT_LYZR_AGENT_ID"); cannot discover tools from an agent\'s own configuration.',
		);
		return;
	}

	const res = await client.getAgent(config.agentId);
	if (!res.ok) {
		stats.errors.push(`agent "${config.agentId}": ${res.error}`);
		logger.warn(`lyzr-tools: failed to fetch Lyzr agent "${config.agentId}": ${res.error}`);
		return;
	}

	const agent = res.data as { tool_configs?: Array<Record<string, any>> } | undefined;
	const toolConfigs = Array.isArray(agent?.tool_configs) ? agent!.tool_configs! : [];
	stats.agentToolConfigsFound = toolConfigs.length;

	for (const toolConfig of toolConfigs) {
		const rawToolName = String(toolConfig.tool_name ?? "").trim();
		const toolSource = String(toolConfig.tool_source ?? "");
		const actionNames: string[] = Array.isArray(toolConfig.action_names) ? toolConfig.action_names.map(String) : [];
		const credentialId = toolConfig.credential_id ? String(toolConfig.credential_id) : undefined;
		const providerUuid = toolConfig.provider_uuid ? String(toolConfig.provider_uuid) : undefined;

		// Best-effort provider key inferred from the "<provider>-<label>"
		// naming convention observed on a real account (e.g.
		// "gmail-Akshat Gmail Integration" -> "gmail"). Not a guaranteed
		// contract — only used for auth wording / dedupe matching below.
		const inferredProvider = rawToolName.split("-")[0]?.trim().toLowerCase() || undefined;
		const connectedAccount = inferredProvider ? connected.get(inferredProvider) : undefined;

		const authorized = Boolean(credentialId) || (connectedAccount?.authorized ?? false);
		if (!authorized) stats.unauthorized++;

		for (const actionName of actionNames) {
			const trimmedAction = actionName.trim();
			if (!trimmedAction) continue;

			const toolName = normalizeToolName(trimmedAction);
			if (seenNames.has(toolName)) continue;
			seenNames.add(toolName);
			stats.agentActionsFound++;

			tools.push({
				rawName: trimmedAction,
				toolName,
				displayName: `${rawToolName || "Lyzr tool"}: ${trimmedAction}`,
				description: `${trimmedAction} action via "${rawToolName}", executed through Lyzr's pre-authorized credentials.`,
				inputSchema: { properties: {} },
				execSource: "agent",
				provider: inferredProvider,
				toolSource,
				actionName: trimmedAction,
				actionNames,
				providerUuid: providerUuid ?? connectedAccount?.providerUuid,
				credentialId: credentialId ?? connectedAccount?.credentialId,
				rawToolConfig: toolConfig,
				authorized,
				authUrl: connectedAccount?.authUrl,
			});
		}
	}
}

// ── MCP discovery ───────────────────────────────────────────────────────

async function discoverMcpTools(
	client: LyzrClient,
	tools: LyzrDiscoveredTool[],
	seenNames: Set<string>,
	stats: DiscoveryStats,
	logger: Logger,
): Promise<void> {
	const serversRes = await client.listMcpServers();
	if (!serversRes.ok) {
		stats.errors.push(`mcp servers: ${serversRes.error}`);
		logger.warn(`lyzr-tools: failed to list MCP servers: ${serversRes.error}`);
		return;
	}

	const servers = extractList(serversRes.data, ["servers", "data", "items"]);
	for (const server of servers) {
		const serverId = String(server.id ?? server.server_id ?? "").trim();
		if (!serverId) continue;
		stats.mcpServersQueried++;

		const toolsRes = await client.listMcpServerTools(serverId);
		if (!toolsRes.ok) {
			stats.errors.push(`mcp server "${serverId}": ${toolsRes.error}`);
			logger.warn(`lyzr-tools: failed to list tools for MCP server "${serverId}": ${toolsRes.error}`);
			continue;
		}

		const data = toolsRes.data as { server_name?: string; tools?: Array<Record<string, any>> } | undefined;
		const serverName = data?.server_name ?? server.name ?? serverId;
		const providerKey = normalizeProviderKey(String(serverName));

		// auth_type "oauth" without an active token means the server is not
		// yet authorized; everything else (no_auth, api_key) is treated as
		// already usable since Lyzr owns those credentials server-side.
		const authType = String(server.auth_type ?? "").toLowerCase();
		const hasToken = Boolean(server.has_active_token ?? server.hasActiveToken);
		const serverAuthorized = authType !== "oauth" || hasToken;

		for (const t of data?.tools ?? []) {
			const actionName = String(t.name ?? "").trim();
			if (!actionName) continue;

			const toolName = normalizeToolName(`mcp_${serverName}_${actionName}`);
			if (seenNames.has(toolName)) continue;
			seenNames.add(toolName);
			stats.mcpToolsFound++;

			if (!serverAuthorized) stats.unauthorized++;

			tools.push({
				rawName: actionName,
				toolName,
				displayName: String(t.display_name ?? actionName),
				description: String(t.description ?? `${actionName} tool on MCP server "${serverName}", executed through Lyzr.`),
				inputSchema: normalizeInputSchema(t.input_schema),
				execSource: "mcp",
				provider: providerKey,
				serverId,
				actionName,
				authorized: serverAuthorized,
			});
		}
	}
}

// ── Connected accounts ──────────────────────────────────────────────────

async function fetchConnectedAccounts(
	client: LyzrClient,
	config: ResolvedConfig,
	stats: DiscoveryStats,
	logger: Logger,
): Promise<Map<string, ConnectedAccount>> {
	const map = new Map<string, ConnectedAccount>();
	if (!config.userId) {
		logger.warn(
			'lyzr-tools: user_id is not configured (config "user_id" / env "LYZR_USER_ID"); connected-account status cannot be cross-referenced, so authorization will rely solely on each tool_config\'s own credential_id.',
		);
		return map;
	}

	const res = await client.listConnectedAccounts(config.userId);
	if (!res.ok) {
		stats.errors.push(`connected_accounts: ${res.error}`);
		logger.warn(`lyzr-tools: failed to list connected accounts: ${res.error}`);
		return map;
	}

	const entries = extractList(res.data, ["accounts", "data", "items", "connected_accounts"]);
	for (const entry of entries) {
		const provider = String(
			entry.provider ?? entry.app_id ?? entry.provider_name ?? entry.tool_source ?? "",
		).toLowerCase();
		if (!provider) continue;

		const status = entry.status ? String(entry.status).toLowerCase() : undefined;
		map.set(provider, {
			authorized: status ? status !== "expired" && status !== "revoked" && status !== "disconnected" : true,
			credentialId: entry.credential_id ?? entry.id,
			providerUuid: entry.provider_uuid,
			authUrl: entry.auth_url,
		});
	}
	return map;
}

// ── Helpers ──────────────────────────────────────────────────────────────

function extractList(data: unknown, keys: string[]): Array<Record<string, any>> {
	if (Array.isArray(data)) return data as Array<Record<string, any>>;
	if (data && typeof data === "object") {
		for (const key of keys) {
			const v = (data as Record<string, any>)[key];
			if (Array.isArray(v)) return v;
		}
	}
	return [];
}

function normalizeInputSchema(schema: unknown): { properties: Record<string, any>; required?: string[] } {
	if (Array.isArray(schema)) {
		// OpenAI/ACI-style parameter list: [{ name, type, description, required? }].
		const properties: Record<string, any> = {};
		const required: string[] = [];
		for (const p of schema as Array<Record<string, any>>) {
			if (!p || !p.name) continue;
			properties[p.name] = { type: p.type ?? "string", description: p.description ?? "" };
			if (p.required) required.push(p.name);
		}
		return required.length > 0 ? { properties, required } : { properties };
	}
	if (schema && typeof schema === "object" && "properties" in (schema as Record<string, unknown>)) {
		return schema as { properties: Record<string, any>; required?: string[] };
	}
	return { properties: {} };
}
