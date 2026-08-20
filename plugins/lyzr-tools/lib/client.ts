// Thin HTTP client over the Lyzr Agent API "/v3" tool-related endpoints.
//
// Endpoints and auth scheme (x-api-key header, per the `APIKeyHeader`
// securityScheme) are confirmed against the Lyzr Swagger document referenced
// in docs/lyzr-tool-auth-rca.md. Response shapes for /v3/tools/,
// /v3/tools/all/user, and /v3/providers/tools/all are documented as opaque
// generic objects in that Swagger, so callers of this client must normalize
// them defensively (see lib/discover.ts) rather than trusting a fixed shape.
//
// The client never throws on HTTP/network failure — every method resolves
// to a LyzrResult so callers can degrade gracefully instead of crashing
// plugin discovery (see docs/lyzr-tool-bridge-test-cases.md TC-C09).

import type { ResolvedConfig } from "./types.ts";

export interface LyzrResult<T = unknown> {
	ok: boolean;
	status?: number;
	data?: T;
	error?: string;
}

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

export interface LyzrClient {
	getAgent(agentId: string): Promise<LyzrResult>;
	listUserTools(): Promise<LyzrResult>;
	listAllUserTools(): Promise<LyzrResult>;
	listConnectedAccounts(userId: string): Promise<LyzrResult>;
	listProviderActions(
		providerIdentifier: string,
		opts?: { toolSource?: string; appId?: string },
	): Promise<LyzrResult>;
	listAllProviderTools(): Promise<LyzrResult>;
	listAciTools(): Promise<LyzrResult>;
	listMcpServers(): Promise<LyzrResult>;
	listMcpServerTools(serverId: string): Promise<LyzrResult>;
	executeInferenceTool(payload: Record<string, unknown>): Promise<LyzrResult>;
	executeMcpTool(payload: Record<string, unknown>): Promise<LyzrResult>;
	initiateMcpOAuth(serverId: string): Promise<LyzrResult>;
	getMcpOAuthStatus(serverId: string, state: string): Promise<LyzrResult>;
}

function extractErrorMessage(data: unknown): string | undefined {
	if (!data) return undefined;
	if (typeof data === "string") return data;
	if (typeof data === "object") {
		const obj = data as Record<string, unknown>;
		if (typeof obj.error === "string") return obj.error;
		if (typeof obj.message === "string") return obj.message;
		if (typeof obj.detail === "string") return obj.detail;
	}
	return undefined;
}

export function createLyzrClient(
	config: Pick<ResolvedConfig, "apiKey" | "baseUrl" | "timeoutMs">,
	fetchImpl: FetchLike = fetch as FetchLike,
): LyzrClient {
	const baseUrl = config.baseUrl.replace(/\/+$/, "");
	const timeoutMs = config.timeoutMs > 0 ? config.timeoutMs : 10_000;

	async function request<T = unknown>(
		method: string,
		path: string,
		opts: { query?: Record<string, string | undefined>; body?: unknown } = {},
	): Promise<LyzrResult<T>> {
		const url = new URL(baseUrl + path);
		if (opts.query) {
			for (const [key, value] of Object.entries(opts.query)) {
				if (value !== undefined && value !== "") url.searchParams.set(key, value);
			}
		}

		const controller = new AbortController();
		const timer = setTimeout(() => controller.abort(), timeoutMs);

		try {
			const res = await fetchImpl(url.toString(), {
				method,
				headers: {
					"x-api-key": config.apiKey,
					...(opts.body !== undefined ? { "Content-Type": "application/json" } : {}),
				},
				body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
				signal: controller.signal,
			});

			const text = await res.text();
			let data: any;
			if (text) {
				try {
					data = JSON.parse(text);
				} catch {
					data = text;
				}
			}

			if (!res.ok) {
				return {
					ok: false,
					status: res.status,
					data,
					error: extractErrorMessage(data) ?? `HTTP ${res.status}`,
				};
			}
			return { ok: true, status: res.status, data };
		} catch (err: any) {
			const message = err?.name === "AbortError" ? "Request timed out" : (err?.message ?? String(err));
			return { ok: false, error: message };
		} finally {
			clearTimeout(timer);
		}
	}

	return {
		getAgent: (agentId: string) => request("GET", `/v3/agents/${encodeURIComponent(agentId)}`),

		listUserTools: () => request("GET", "/v3/tools/"),

		listAllUserTools: () => request("GET", "/v3/tools/all/user"),

		listConnectedAccounts: (userId: string) =>
			request("GET", "/v3/tools/credentials/connected_accounts", { query: { user_id: userId } }),

		listProviderActions: (providerIdentifier: string, opts: { toolSource?: string; appId?: string } = {}) =>
			request("GET", `/v3/providers/tools/actions/${encodeURIComponent(providerIdentifier)}`, {
				query: { tool_source: opts.toolSource, app_id: opts.appId },
			}),

		listAllProviderTools: () => request("GET", "/v3/providers/tools/all"),

		listAciTools: () => request("GET", "/v3/providers/lyzr/aci-tools"),

		listMcpServers: () => request("GET", "/v3/tools/mcp/servers"),

		listMcpServerTools: (serverId: string) =>
			request("GET", `/v3/tools/mcp/servers/${encodeURIComponent(serverId)}/tools`),

		executeInferenceTool: (payload: Record<string, unknown>) =>
			request("POST", "/v3/inference/tools/execute", { body: payload }),

		executeMcpTool: (payload: Record<string, unknown>) =>
			request("POST", "/v3/tools/mcp/tools/execute", { body: payload }),

		initiateMcpOAuth: (serverId: string) =>
			request("POST", `/v3/tools/mcp/servers/${encodeURIComponent(serverId)}/oauth/initiate`),

		getMcpOAuthStatus: (serverId: string, state: string) =>
			request("GET", `/v3/tools/mcp/servers/${encodeURIComponent(serverId)}/oauth/status`, {
				query: { state },
			}),
	};
}
