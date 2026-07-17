// Phase 3: Tool execution proxy.
//
// Routes a tool call to the correct Lyzr execution surface and normalizes
// the result into gitagent's { text, details } tool-result shape. Three
// outcomes are distinguished:
//
//   - authorization_required: the provider/MCP server is not (yet)
//     authorized in Lyzr. The model is told to ask the user to authorize in
//     Lyzr — never to collect local credentials (RCA "Assurance Model").
//   - error: execution failed for any other reason.
//   - success: the call went through; Lyzr's result is returned verbatim.
//
// All `details` payloads are passed through redactSecrets() before being
// returned, so no credential/token/secret field can leak into logs or model
// context (docs/lyzr-tool-bridge-test-cases.md TC-D03).

import type { LyzrClient } from "./client.ts";
import type { LyzrDiscoveredTool, ResolvedConfig, ToolCallResult } from "./types.ts";
import { redactSecrets } from "./redact.ts";

const AUTH_HINT_RE =
	/(unauthor|not\s*connect|not\s*authoriz|authoriz(e|ation)\s*required|no\s*credential|missing\s*credential|reconnect|expired\s*token|invalid_grant|please\s*(re)?authenticate)/i;

export function detectAuthRequired(status: number | undefined, body: unknown): boolean {
	if (status === 401 || status === 403) return true;
	if (body === undefined || body === null) return false;
	const text = typeof body === "string" ? body : safeStringify(body);
	return AUTH_HINT_RE.test(text);
}

export async function executeLyzrTool(
	client: LyzrClient,
	config: ResolvedConfig,
	tool: LyzrDiscoveredTool,
	args: Record<string, unknown>,
): Promise<ToolCallResult> {
	// Known-unauthorized at discovery time: don't even make the call, and
	// never fall back to asking for local credentials.
	if (!tool.authorized) {
		return authRequiredResult(tool);
	}

	if (tool.execSource === "mcp") {
		return executeMcp(client, tool, args);
	}
	return executeAgentTool(client, config, tool, args);
}

async function executeMcp(
	client: LyzrClient,
	tool: LyzrDiscoveredTool,
	args: Record<string, unknown>,
): Promise<ToolCallResult> {
	const res = await client.executeMcpTool({
		server_id: tool.serverId,
		tool_name: tool.actionName ?? tool.rawName,
		arguments: args,
	});

	if (!res.ok) {
		if (detectAuthRequired(res.status, res.data ?? res.error)) return authRequiredResult(tool);
		return errorResult(tool, res.error ?? "MCP tool execution failed");
	}

	const data = res.data as { success?: boolean; error?: string | null; result?: unknown } | undefined;
	if (data && data.success === false) {
		if (AUTH_HINT_RE.test(data.error ?? "")) return authRequiredResult(tool);
		return errorResult(tool, data.error ?? "MCP tool execution failed");
	}

	return successResult(tool, data?.result);
}

async function executeAgentTool(
	client: LyzrClient,
	config: ResolvedConfig,
	tool: LyzrDiscoveredTool,
	args: Record<string, unknown>,
): Promise<ToolCallResult> {
	const traceId = `lyzr-tools-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;

	// NOTE: the exact pairing between the top-level `tool_name` and
	// `ToolConfig` fields is not fully pinned down by the Swagger response
	// schema for POST /v3/inference/tools/execute (flagged as a "Remaining
	// API Alignment Item" in docs/lyzr-tool-auth-rca.md). This mapping is
	// our best-effort interpretation: `tool_name` is the specific action to
	// invoke, and `tool_configs[0]` describes the provider/credential
	// context that action runs under.
	const res = await client.executeInferenceTool({
		agent_id: config.agentId || undefined,
		tool_name: tool.actionName ?? tool.rawName,
		tool_configs: [
			{
				tool_name: tool.toolSource ?? tool.provider ?? tool.rawName,
				tool_source: tool.toolSource ?? tool.provider ?? "unknown",
				action_names:
					tool.actionNames && tool.actionNames.length > 0 ? tool.actionNames : [tool.actionName ?? tool.rawName],
				persist_auth: config.persistAuth,
				provider_uuid: tool.providerUuid,
				credential_id: tool.credentialId,
			},
		],
		arguments: args,
		trace_id: traceId,
	});

	if (!res.ok) {
		if (detectAuthRequired(res.status, res.data ?? res.error)) return authRequiredResult(tool);
		return errorResult(tool, res.error ?? `Lyzr tool execution failed (HTTP ${res.status ?? "unknown"})`);
	}

	const data = res.data as { result?: unknown; trace_id?: string } | undefined;
	return successResult(tool, data?.result, data?.trace_id ?? traceId);
}

// ── Result builders ──────────────────────────────────────────────────────

function authRequiredResult(tool: LyzrDiscoveredTool): ToolCallResult {
	return {
		text: `Authorization required for ${tool.displayName}. Ask the user to authorize "${tool.provider ?? tool.displayName}" in Lyzr${tool.authUrl ? `: ${tool.authUrl}` : "."} Do not ask the user for local API keys, passwords, or OAuth tokens for this tool.`,
		details: redactSecrets({
			status: "authorization_required",
			provider: tool.provider ?? null,
			tool: tool.toolName,
			auth_url: tool.authUrl ?? null,
		}),
	};
}

function errorResult(tool: LyzrDiscoveredTool, message: string): ToolCallResult {
	return {
		text: `Lyzr tool "${tool.displayName}" failed: ${message}`,
		details: redactSecrets({ status: "error", tool: tool.toolName, error: message }),
	};
}

function successResult(tool: LyzrDiscoveredTool, result: unknown, traceId?: string): ToolCallResult {
	const text = typeof result === "string" ? result : result === undefined ? "Done." : safeStringify(result);
	return {
		text,
		details: redactSecrets({ status: "success", tool: tool.toolName, trace_id: traceId ?? null, result }),
	};
}

function safeStringify(value: unknown): string {
	try {
		return JSON.stringify(value);
	} catch {
		return String(value);
	}
}
