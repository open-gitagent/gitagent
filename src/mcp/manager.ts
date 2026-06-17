import { buildTool } from "../tool-factory.js";
import { interpolateEnv } from "../env-utils.js";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { Client } from "@modelcontextprotocol/sdk/client/index.js";
import type { McpServerConfig, McpStdioServerConfig, McpSetupResult } from "./types.js";

const DEFAULT_TIMEOUT_MS = 30000;

/** A live connection to one MCP server. */
interface McpConnection {
	name: string;
	client: Client;
	close: () => Promise<void>;
}

function withTimeout<T>(op: Promise<T>, ms: number, label: string): Promise<T> {
	return Promise.race([
		op,
		new Promise<T>((_, reject) =>
			setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms),
		),
	]);
}

/**
 * Flatten an MCP tool result (an array of content blocks plus an optional
 * `isError` flag) into the plain string that an AgentTool's execute() returns.
 * Binary blocks are summarized rather than inlined to protect the token budget.
 */
export function flattenToolResult(result: any): string {
	const blocks: any[] = Array.isArray(result?.content) ? result.content : [];
	const parts: string[] = [];
	for (const block of blocks) {
		switch (block?.type) {
			case "text":
				parts.push(String(block.text ?? ""));
				break;
			case "image":
				parts.push(`[image: ${block.mimeType || "unknown"}, data omitted]`);
				break;
			case "audio":
				parts.push(`[audio: ${block.mimeType || "unknown"}, data omitted]`);
				break;
			case "resource": {
				const res = block.resource ?? {};
				if (typeof res.text === "string") parts.push(res.text);
				else parts.push(`[resource: ${res.uri || "unknown"} (${res.mimeType || "binary"})]`);
				break;
			}
			case "resource_link":
				parts.push(`[resource_link: ${block.uri || "unknown"}]`);
				break;
			default:
				if (block?.text) parts.push(String(block.text));
				break;
		}
	}
	let text = parts.join("\n");
	// Some tools (those with an outputSchema) return only `structuredContent`
	// with no text blocks — fall back to its JSON so the model isn't handed "".
	if (!text && result?.structuredContent !== undefined) {
		try {
			text = JSON.stringify(result.structuredContent);
		} catch {
			/* leave text empty if not serializable */
		}
	}
	return result?.isError ? `Error: ${text}` : text;
}

/**
 * Make a tool name safe for LLM provider APIs, which require names to match
 * roughly `^[a-zA-Z0-9_-]{1,64}$` (OpenAI/Anthropic). Invalid characters become
 * `_`; names longer than 64 chars are truncated. Collisions after truncation
 * are caught by the existing-name check in setupMcp.
 */
function sanitizeToolName(name: string): string {
	const cleaned = name.replace(/[^a-zA-Z0-9_-]/g, "_");
	return cleaned.length > 64 ? cleaned.slice(0, 64) : cleaned;
}

/** List every tool a server offers, following pagination cursors to the end. */
async function listAllTools(client: Client): Promise<any[]> {
	const all: any[] = [];
	let cursor: string | undefined;
	do {
		const res = await client.listTools(cursor ? { cursor } : undefined);
		all.push(...res.tools);
		cursor = res.nextCursor;
	} while (cursor);
	return all;
}

/**
 * List a connected server's tools and convert each into an AgentTool.
 * Tools are namespaced `<server>__<tool>` (sanitized for provider name rules)
 * to avoid collisions. Exported so tests can exercise conversion over an
 * in-memory transport without spawning.
 */
export async function buildToolsForConnection(conn: McpConnection): Promise<AgentTool<any>[]> {
	const tools = await listAllTools(conn.client);
	return tools.map((t) =>
		buildTool({
			name: sanitizeToolName(`${conn.name}__${t.name}`),
			description: t.description || `MCP tool ${t.name} from ${conn.name}`,
			// MCP inputSchema is a JSON-Schema object ({type, properties, required}).
			// buildTypeboxSchema converts it recursively (integers, enums, typed
			// arrays, nested objects); unknown shapes degrade to Type.Any.
			parameters: (t.inputSchema as Record<string, any>) ?? {},
			execute: async (args: any, signal?: AbortSignal) => {
				try {
					// Forward the agent's abort signal so cancelling the turn cancels
					// the in-flight MCP request. Call with the original (unsanitized) name.
					const result = await conn.client.callTool(
						{ name: t.name, arguments: args ?? {} },
						undefined,
						{ signal },
					);
					return flattenToolResult(result);
				} catch (err: any) {
					// Protocol-level errors (e.g. -32602) throw; surface them as a
					// string so the agent can read and recover instead of crashing.
					return `Error: ${err?.message || err}`;
				}
			},
			// We can't know a remote tool's semantics — fail closed.
			metadata: { isConcurrencySafe: false, isReadOnly: false, isDestructive: false },
		}),
	);
}

/** Connect to a single server. Fail-soft: logs and returns null on any error. */
async function connectServer(name: string, rawCfg: McpServerConfig): Promise<McpConnection | null> {
	const cfg = interpolateEnv(rawCfg);
	const timeoutMs = cfg.timeoutMs ?? DEFAULT_TIMEOUT_MS;
	try {
		const { Client } = await import("@modelcontextprotocol/sdk/client/index.js");

		let transport: any;
		if (cfg.type === "http") {
			if (!cfg.url) throw new Error("http server requires a url");
			const { StreamableHTTPClientTransport } = await import(
				"@modelcontextprotocol/sdk/client/streamableHttp.js"
			);
			transport = new StreamableHTTPClientTransport(new URL(cfg.url), {
				requestInit: cfg.headers ? { headers: cfg.headers } : undefined,
			});
		} else if (cfg.type === "sse") {
			if (!cfg.url) throw new Error("sse server requires a url");
			const { SSEClientTransport } = await import("@modelcontextprotocol/sdk/client/sse.js");
			transport = new SSEClientTransport(new URL(cfg.url), {
				requestInit: cfg.headers ? { headers: cfg.headers } : undefined,
			});
		} else {
			const stdio = cfg as McpStdioServerConfig;
			if (!stdio.command) throw new Error("stdio server requires a command");
			const { StdioClientTransport, getDefaultEnvironment } = await import(
				"@modelcontextprotocol/sdk/client/stdio.js"
			);
			transport = new StdioClientTransport({
				command: stdio.command,
				args: stdio.args ?? [],
				env: { ...getDefaultEnvironment(), ...(stdio.env ?? {}) },
				cwd: stdio.cwd,
			});
		}

		const client = new Client({ name: "gitagent", version: "1.5.2" }, { capabilities: {} });
		try {
			await withTimeout(client.connect(transport), timeoutMs, `[mcp:${name}] connect`);
		} catch (connectErr) {
			// connect() spawns the child process / opens the socket. If it fails or
			// times out mid-handshake, close the transport so we don't leak the
			// spawned process or connection.
			try { await transport?.close?.(); } catch { /* best-effort */ }
			throw connectErr;
		}

		return {
			name,
			client,
			close: async () => {
				try {
					await client.close();
				} catch {
					/* best-effort */
				}
			},
		};
	} catch (err: any) {
		console.warn(`[mcp:${name}] failed to connect: ${err?.message || err} — skipping`);
		return null;
	}
}

/**
 * Connect to all configured MCP servers, register their tools, and return an
 * idempotent cleanup. Servers connect in parallel and independently fail-soft —
 * one bad server never blocks the others. Returns immediately with no work (and
 * without loading the MCP SDK) when no servers are configured.
 *
 * @param servers           merged server map (manifest + SDK options)
 * @param existingToolNames running set of tool names already registered; used
 *                          for collision detection and updated in place.
 */
export async function setupMcp(
	servers: Record<string, McpServerConfig> | undefined,
	existingToolNames: Set<string>,
): Promise<McpSetupResult> {
	const entries = Object.entries(servers ?? {});
	if (entries.length === 0) {
		return { tools: [], cleanup: async () => {} };
	}

	const settled = await Promise.allSettled(
		entries.map(([name, cfg]) => connectServer(name, cfg)),
	);
	const connections = settled
		.map((s) => (s.status === "fulfilled" ? s.value : null))
		.filter((c): c is McpConnection => c !== null);

	const tools: AgentTool<any>[] = [];
	for (const conn of connections) {
		let built: AgentTool<any>[];
		try {
			built = await withTimeout(
				buildToolsForConnection(conn),
				DEFAULT_TIMEOUT_MS,
				`[mcp:${conn.name}] listTools`,
			);
		} catch (err: any) {
			console.warn(`[mcp:${conn.name}] failed to list tools: ${err?.message || err} — skipping`);
			await conn.close();
			continue;
		}
		for (const tool of built) {
			if (existingToolNames.has(tool.name)) {
				console.warn(`[mcp:${conn.name}] tool "${tool.name}" collides with an existing tool — skipping`);
				continue;
			}
			existingToolNames.add(tool.name);
			tools.push(tool);
		}
	}

	let closed = false;
	const cleanup = async () => {
		if (closed) return;
		closed = true;
		await Promise.allSettled(connections.map((c) => c.close()));
	};

	return { tools, cleanup };
}
