// Converts Composio tools into GCToolDefinition[] for injection into query()

import type { GCToolDefinition } from "../sdk-types.js";
import { ComposioClient, type ComposioToolkit, type ComposioConnection, type ComposioTool } from "./client.js";

interface ComposioAdapterOptions {
	apiKey: string;
	userId?: string;
}

export class ComposioAdapter {
	private client: ComposioClient;
	private userId: string;
	private cachedTools: GCToolDefinition[] | null = null;
	private cacheExpiry = 0;
	private static CACHE_TTL = 30_000; // 30s

	constructor(opts: ComposioAdapterOptions) {
		this.client = new ComposioClient(opts.apiKey);
		this.userId = opts.userId ?? "default";
	}

	// Core — returns all tools for connected toolkits (cached)
	async getTools(): Promise<GCToolDefinition[]> {
		const now = Date.now();
		if (this.cachedTools && now < this.cacheExpiry) return this.cachedTools;

		const connections = await this.client.listConnections(this.userId);
		if (connections.length === 0) return [];

		// Deduplicate toolkit slugs
		const slugs = [...new Set(connections.map((c) => c.toolkitSlug))];

		// Fetch tools for each connected toolkit in parallel
		const toolsBySlug = await Promise.all(
			slugs.map((slug) => this.client.listTools(slug).catch(() => [] as ComposioTool[])),
		);

		const tools: GCToolDefinition[] = [];
		for (const toolGroup of toolsBySlug) {
			for (const t of toolGroup) {
				tools.push(this.toGCTool(t));
			}
		}

		this.cachedTools = tools;
		this.cacheExpiry = now + ComposioAdapter.CACHE_TTL;
		return tools;
	}

	// Dynamically fetch only the relevant tools for a user query (semantic search)
	async getToolsForQuery(query: string, limit = 10): Promise<GCToolDefinition[]> {
		const connections = await this.client.listConnections(this.userId);
		if (connections.length === 0) return [];

		const slugs = [...new Set(connections.map((c) => c.toolkitSlug))];
		const tools = await this.client.searchTools(query, slugs, limit);

		// Sort: direct-action tools first (SEND, CREATE, LIST), drafts last
		tools.sort((a, b) => {
			const aIsDraft = a.slug.includes("DRAFT");
			const bIsDraft = b.slug.includes("DRAFT");
			if (aIsDraft !== bIsDraft) return aIsDraft ? 1 : -1;
			return 0;
		});

		return tools.map((t) => this.toGCTool(t));
	}

	// Returns deduplicated slugs of all connected toolkits
	async getConnectedToolkitSlugs(): Promise<string[]> {
		const connections = await this.client.listConnections(this.userId);
		return [...new Set(connections.map((c) => c.toolkitSlug))];
	}

	// Management endpoints — proxied for server routes

	async getToolkits(): Promise<ComposioToolkit[]> {
		return this.client.listToolkits(this.userId);
	}

	async connect(
		toolkit: string,
		redirectUrl?: string,
	): Promise<{ connectionId: string; redirectUrl: string }> {
		return this.client.initiateConnection(toolkit, this.userId, redirectUrl);
	}

	async getConnections(): Promise<ComposioConnection[]> {
		return this.client.listConnections(this.userId);
	}

	async disconnect(connectionId: string): Promise<void> {
		await this.client.deleteConnection(connectionId);
		// Invalidate cache so tools refresh on next query
		this.cachedTools = null;
	}

	// ── Private ────────────────────────────────────────────────────────

	private toGCTool(t: ComposioTool): GCToolDefinition {
		const safeName = `composio_${t.toolkitSlug}_${t.slug}`.replace(/[^a-zA-Z0-9_]/g, "_");
		let description = `[Composio/${t.toolkitSlug}] ${t.description}`;
		if (t.slug.includes("SEND_EMAIL")) {
			description += " — USE THIS to send emails directly.";
		} else if (t.slug.includes("CREATE_EMAIL_DRAFT")) {
			description += " — Only use when the user explicitly asks for a draft.";
		}
		return {
			name: safeName,
			description,
			inputSchema: t.parameters,
			handler: async (args: any) => {
				const result = await this.client.executeTool(t.slug, this.userId, args);
				return typeof result === "string" ? result : JSON.stringify(result);
			},
		};
	}
}
