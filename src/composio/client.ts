// Composio REST API v3 client — zero dependencies, uses native fetch()

const BASE_URL = "https://backend.composio.dev/api/v3";

// ── Types ────────────────────────────────────────────────────────────

export interface ComposioToolkit {
	slug: string;
	name: string;
	description: string;
	logo: string;
	authSchemes: string[];
	noAuth: boolean;
	connected: boolean;
}

export interface ComposioConnection {
	id: string;
	toolkitSlug: string;
	status: string;
	createdAt: string;
}

export interface ComposioTool {
	name: string;
	slug: string;
	description: string;
	toolkitSlug: string;
	parameters: Record<string, any>;
}

// ── Client ───────────────────────────────────────────────────────────

export class ComposioClient {
	private apiKey: string;
	// Cache auth config IDs so we don't recreate them every connect
	private authConfigCache = new Map<string, string>();

	constructor(apiKey: string) {
		this.apiKey = apiKey;
	}

	// List available toolkits, optionally merging connection status for a user
	async listToolkits(userId?: string): Promise<ComposioToolkit[]> {
		const resp = await this.request<any>("GET", "/toolkits");

		const toolkits: any[] = Array.isArray(resp) ? resp : (resp.items ?? resp.toolkits ?? []);

		let connectedSlugs = new Set<string>();
		if (userId) {
			try {
				const conns = await this.listConnections(userId);
				connectedSlugs = new Set(conns.map((c) => c.toolkitSlug));
			} catch {
				// If connections fail, just show all as disconnected
			}
		}

		return toolkits.map((tk: any) => ({
			slug: tk.slug ?? "",
			name: tk.name ?? tk.slug ?? "",
			description: tk.meta?.description ?? tk.description ?? "",
			logo: tk.meta?.logo ?? tk.logo ?? "",
			authSchemes: tk.auth_schemes ?? [],
			noAuth: tk.no_auth ?? false,
			connected: connectedSlugs.has(tk.slug ?? ""),
		}));
	}

	// List tools for a specific toolkit
	async listTools(toolkitSlug: string): Promise<ComposioTool[]> {
		const resp = await this.request<any>(
			"GET",
			`/tools?toolkit_slug=${encodeURIComponent(toolkitSlug)}`,
		);

		const tools: any[] = Array.isArray(resp) ? resp : (resp.items ?? resp.tools ?? []);

		return tools.map((t: any) => ({
			name: t.name ?? t.enum ?? "",
			slug: t.slug ?? t.enum ?? t.name ?? "",
			description: t.description ?? "",
			toolkitSlug,
			parameters: t.parameters ?? t.inputParameters ?? {},
		}));
	}

	// Get or create an auth config for a toolkit (needed before creating a connection)
	async getOrCreateAuthConfig(toolkitSlug: string): Promise<string> {
		// Check cache first
		const cached = this.authConfigCache.get(toolkitSlug);
		if (cached) return cached;

		// Check if one already exists
		const existing = await this.request<any>(
			"GET",
			`/auth_configs?toolkit_slug=${encodeURIComponent(toolkitSlug)}`,
		);
		const items: any[] = existing.items ?? [];
		if (items.length > 0) {
			const id = items[0].id ?? items[0].auth_config?.id;
			if (id) {
				this.authConfigCache.set(toolkitSlug, id);
				return id;
			}
		}

		// Create a new one with Composio-managed auth
		const created = await this.request<any>("POST", "/auth_configs", {
			toolkit: { slug: toolkitSlug },
			auth_scheme: "OAUTH2",
			use_composio_auth: true,
		});

		const id = created.auth_config?.id ?? created.id ?? "";
		if (id) this.authConfigCache.set(toolkitSlug, id);
		return id;
	}

	// Start OAuth connection flow (two-step: ensure auth config, then create connection)
	async initiateConnection(
		toolkitSlug: string,
		userId: string,
		redirectUrl?: string,
	): Promise<{ connectionId: string; redirectUrl: string }> {
		const authConfigId = await this.getOrCreateAuthConfig(toolkitSlug);
		if (!authConfigId) {
			throw new Error(`Failed to get auth config for toolkit: ${toolkitSlug}`);
		}

		const body: Record<string, any> = {
			auth_config: { id: authConfigId },
			connection: {
				user_id: userId,
				...(redirectUrl ? { callback_url: redirectUrl } : {}),
			},
		};

		const resp = await this.request<any>("POST", "/connected_accounts", body);
		return {
			connectionId: resp.id ?? "",
			redirectUrl: resp.redirect_url ?? resp.redirect_uri ?? resp.redirectUrl ?? resp.redirectUri ?? "",
		};
	}

	// List active connections for a user
	async listConnections(userId: string): Promise<ComposioConnection[]> {
		const resp = await this.request<any>(
			"GET",
			`/connected_accounts?user_ids=${encodeURIComponent(userId)}&statuses=ACTIVE`,
		);

		const items: any[] = Array.isArray(resp) ? resp : (resp.items ?? resp.connections ?? []);
		return items.map((c: any) => ({
			id: c.id ?? "",
			toolkitSlug: c.toolkit?.slug ?? c.toolkit_slug ?? c.appUniqueId ?? c.integrationId ?? "",
			status: c.status ?? "ACTIVE",
			createdAt: c.createdAt ?? c.created_at ?? "",
		}));
	}

	// Delete a connection
	async deleteConnection(id: string): Promise<void> {
		await this.request("DELETE", `/connected_accounts/${encodeURIComponent(id)}`);
	}

	// Execute a tool action
	async executeTool(
		toolSlug: string,
		userId: string,
		params: Record<string, any>,
		connectedAccountId?: string,
	): Promise<any> {
		const body: Record<string, any> = {
			arguments: params,
			user_id: userId,
		};
		if (connectedAccountId) body.connected_account_id = connectedAccountId;

		return this.request("POST", `/tools/execute/${encodeURIComponent(toolSlug)}`, body);
	}

	// ── Private ────────────────────────────────────────────────────────

	private async request<T>(method: string, path: string, body?: any): Promise<T> {
		const url = `${BASE_URL}${path}`;
		const headers: Record<string, string> = {
			"x-api-key": this.apiKey,
			"Accept": "application/json",
		};
		if (body) headers["Content-Type"] = "application/json";

		const resp = await fetch(url, {
			method,
			headers,
			body: body ? JSON.stringify(body) : undefined,
		});

		if (!resp.ok) {
			const text = await resp.text().catch(() => "");
			throw new Error(`Composio API ${method} ${path} failed (${resp.status}): ${text}`);
		}

		if (resp.status === 204) return undefined as T;
		return resp.json() as Promise<T>;
	}
}
