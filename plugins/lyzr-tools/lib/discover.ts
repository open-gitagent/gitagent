// Phase 2: Tool discovery.
//
// Discovers tools from two Swagger-confirmed, typed-enough sources:
//
//  1. Provider/action discovery — GET /v3/providers/tools/actions/{provider}
//     for each configured provider (e.g. "gmail", "slack"). This is the
//     primary path for connected-app tools per docs/lyzr-tool-auth-rca.md.
//  2. MCP server tools — GET /v3/tools/mcp/servers + .../{server_id}/tools,
//     which *is* fully typed in Swagger (MCPServerListResponse /
//     ToolsListResponse).
//
// GET /v3/tools/ and /v3/tools/all/user are intentionally not used as a
// registration source: their Swagger response schema is a generic `{}`
// object, so there is no reliable field to normalize into a callable tool.
// They remain available on the client for future use once Lyzr documents a
// concrete shape (see RCA "Remaining API Alignment Item").
//
// Connected-account status (GET /v3/tools/credentials/connected_accounts)
// is cross-referenced so each discovered tool carries an accurate
// `authorized` flag and, where available, the credential_id/provider_uuid
// needed for execution.

import type { LyzrClient } from "./client.ts";
import type { ConnectedAccount, LyzrDiscoveredTool, Logger, ResolvedConfig } from "./types.ts";
import { normalizeProviderKey, normalizeToolName } from "./normalize.ts";

export interface DiscoveryStats {
	providersQueried: number;
	providerActionsFound: number;
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
		providersQueried: 0,
		providerActionsFound: 0,
		mcpServersQueried: 0,
		mcpToolsFound: 0,
		unauthorized: 0,
		errors: [],
	};
	const tools: LyzrDiscoveredTool[] = [];
	const seenNames = new Set<string>();

	const connected = await fetchConnectedAccounts(client, config, stats, logger);

	await discoverProviderActions(client, config, connected, tools, seenNames, stats, logger);

	if (config.includeMcp) {
		await discoverMcpTools(client, tools, seenNames, stats, logger);
	}

	return { tools, stats };
}

// ── Provider/action discovery ──────────────────────────────────────────

async function discoverProviderActions(
	client: LyzrClient,
	config: ResolvedConfig,
	connected: Map<string, ConnectedAccount>,
	tools: LyzrDiscoveredTool[],
	seenNames: Set<string>,
	stats: DiscoveryStats,
	logger: Logger,
): Promise<void> {
	for (const providerId of config.providers) {
		stats.providersQueried++;
		const res = await client.listProviderActions(providerId);
		if (!res.ok) {
			stats.errors.push(`provider "${providerId}": ${res.error}`);
			logger.warn(`lyzr-tools: failed to list actions for provider "${providerId}": ${res.error}`);
			continue;
		}

		const actions = extractList(res.data, ["actions", "data", "items", "results"]);
		const providerKey = providerId.toLowerCase();
		const connectedAccount = connected.get(providerKey);

		for (const action of actions) {
			const actionName = String(action.name ?? action.action_name ?? action.id ?? "").trim();
			if (!actionName) continue;

			const toolName = normalizeToolName(`${providerId}_${actionName}`);
			if (seenNames.has(toolName)) continue;
			seenNames.add(toolName);
			stats.providerActionsFound++;

			const authorized = connectedAccount?.authorized ?? false;
			if (!authorized) stats.unauthorized++;

			tools.push({
				rawName: actionName,
				toolName,
				displayName: String(action.display_name ?? action.title ?? `${providerId} ${actionName}`).trim(),
				description: String(
					action.description ??
						action.desc ??
						`${actionName} action for ${providerId}, executed through Lyzr's pre-authorized credentials.`,
				),
				inputSchema: normalizeInputSchema(action.input_schema ?? action.parameters ?? action.schema),
				execSource: "agent",
				provider: providerKey,
				toolSource: providerId,
				actionName,
				actionNames: [actionName],
				providerUuid: connectedAccount?.providerUuid ?? action.provider_uuid,
				credentialId: connectedAccount?.credentialId,
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
	if (!config.userId) return map;

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
	if (schema && typeof schema === "object" && "properties" in (schema as Record<string, unknown>)) {
		return schema as { properties: Record<string, any>; required?: string[] };
	}
	return { properties: {} };
}
