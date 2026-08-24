import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { z } from "zod";

// Dynamic imports since the project is ESM and tests run against compiled dist.
let buildToolsForConnection: typeof import("../dist/mcp/manager.js").buildToolsForConnection;
let flattenToolResult: typeof import("../dist/mcp/manager.js").flattenToolResult;
let setupMcp: typeof import("../dist/mcp/manager.js").setupMcp;
let Client: typeof import("@modelcontextprotocol/sdk/client/index.js").Client;
let McpServer: typeof import("@modelcontextprotocol/sdk/server/mcp.js").McpServer;
let InMemoryTransport: typeof import("@modelcontextprotocol/sdk/inMemory.js").InMemoryTransport;

before(async () => {
	const mgr = await import("../dist/mcp/manager.js");
	buildToolsForConnection = mgr.buildToolsForConnection;
	flattenToolResult = mgr.flattenToolResult;
	setupMcp = mgr.setupMcp;
	Client = (await import("@modelcontextprotocol/sdk/client/index.js")).Client;
	McpServer = (await import("@modelcontextprotocol/sdk/server/mcp.js")).McpServer;
	InMemoryTransport = (await import("@modelcontextprotocol/sdk/inMemory.js")).InMemoryTransport;
});

/** Stand up an in-memory MCP server exposing echo/boom/pic tools, return a connected Client. */
async function connectInMemoryServer() {
	const server = new McpServer({ name: "test-server", version: "1.0.0" });

	server.registerTool(
		"echo",
		{ description: "Echo a message", inputSchema: { msg: z.string() } },
		async ({ msg }) => ({ content: [{ type: "text", text: msg }] }),
	);
	server.registerTool(
		"boom",
		{ description: "Always errors", inputSchema: {} },
		async () => ({ content: [{ type: "text", text: "kaboom" }], isError: true }),
	);
	server.registerTool(
		"pic",
		{ description: "Returns an image", inputSchema: {} },
		async () => ({ content: [{ type: "image", data: "AAAA", mimeType: "image/png" }] }),
	);

	const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
	const client = new Client({ name: "test-client", version: "1.0.0" }, { capabilities: {} });
	await Promise.all([client.connect(clientTransport), server.connect(serverTransport)]);
	return { client, server };
}

describe("flattenToolResult", () => {
	it("joins text blocks", () => {
		assert.equal(flattenToolResult({ content: [{ type: "text", text: "a" }, { type: "text", text: "b" }] }), "a\nb");
	});
	it("prefixes Error: when isError is true", () => {
		assert.equal(flattenToolResult({ content: [{ type: "text", text: "bad" }], isError: true }), "Error: bad");
	});
	it("summarizes image blocks without inlining data", () => {
		const out = flattenToolResult({ content: [{ type: "image", data: "AAAA", mimeType: "image/png" }] });
		assert.equal(out, "[image: image/png, data omitted]");
		assert.ok(!out.includes("AAAA"));
	});
	it("handles embedded resource text and links", () => {
		assert.equal(flattenToolResult({ content: [{ type: "resource", resource: { uri: "file://x", text: "hi" } }] }), "hi");
		assert.equal(flattenToolResult({ content: [{ type: "resource_link", uri: "file://y" }] }), "[resource_link: file://y]");
	});
	it("tolerates empty/malformed results", () => {
		assert.equal(flattenToolResult({}), "");
		assert.equal(flattenToolResult(null), "");
	});
	it("falls back to structuredContent JSON when there are no text blocks", () => {
		assert.equal(
			flattenToolResult({ content: [], structuredContent: { count: 42 } }),
			'{"count":42}',
		);
	});
	it("prefers text content over structuredContent when both exist", () => {
		assert.equal(
			flattenToolResult({ content: [{ type: "text", text: "hi" }], structuredContent: { x: 1 } }),
			"hi",
		);
	});
});

describe("buildToolsForConnection", () => {
	it("namespaces tool names and wires execute through callTool", async () => {
		const { client } = await connectInMemoryServer();
		const conn = { name: "srv", client, close: async () => { await client.close(); } };
		const tools = await buildToolsForConnection(conn as any);

		const names = tools.map((t) => t.name).sort();
		assert.deepEqual(names, ["srv__boom", "srv__echo", "srv__pic"]);

		const echo = tools.find((t) => t.name === "srv__echo")!;
		const res = await echo.execute("call-1", { msg: "hello" });
		assert.equal(res.content[0].text, "hello");

		const boom = tools.find((t) => t.name === "srv__boom")!;
		const boomRes = await boom.execute("call-2", {});
		assert.match(boomRes.content[0].text, /^Error: /);

		const pic = tools.find((t) => t.name === "srv__pic")!;
		const picRes = await pic.execute("call-3", {});
		assert.match(picRes.content[0].text, /\[image: image\/png/);

		await conn.close();
	});
});

describe("buildToolsForConnection — pagination & name sanitization", () => {
	it("follows nextCursor across pages and sanitizes/caps tool names", async () => {
		// A mock client that paginates listTools across two pages.
		const longName = "x".repeat(100);
		const mockClient: any = {
			async listTools(params: any) {
				if (!params?.cursor) {
					return { tools: [{ name: "do.it", inputSchema: {} }], nextCursor: "page2" };
				}
				return { tools: [{ name: longName, inputSchema: {} }] }; // no nextCursor → end
			},
			async callTool() {
				return { content: [{ type: "text", text: "ok" }] };
			},
		};
		const tools = await buildToolsForConnection({ name: "srv", client: mockClient, close: async () => {} });

		// both pages collected
		assert.equal(tools.length, 2);
		// invalid char "." sanitized to "_"
		assert.equal(tools[0].name, "srv__do_it");
		// long name capped to 64 and only valid chars
		assert.ok(tools[1].name.length <= 64);
		assert.match(tools[1].name, /^[a-zA-Z0-9_-]+$/);
	});

	it("returns a clean 'Error:' string when callTool throws (protocol error)", async () => {
		const mockClient: any = {
			async listTools() {
				return { tools: [{ name: "bad", inputSchema: {} }] };
			},
			async callTool() {
				throw new Error("MCP error -32602: Invalid arguments");
			},
		};
		const tools = await buildToolsForConnection({ name: "srv", client: mockClient, close: async () => {} });
		const res = await tools[0].execute("c1", {});
		assert.match(res.content[0].text, /^Error: .*-32602/);
	});
});

describe("setupMcp", () => {
	it("returns empty tools and a no-op cleanup when no servers configured", async () => {
		const result = await setupMcp(undefined, new Set());
		assert.deepEqual(result.tools, []);
		// cleanup is idempotent / safe to call repeatedly
		await result.cleanup();
		await result.cleanup();
	});

	it("fails soft on an unreachable server (bad command) without throwing", async () => {
		const existing = new Set<string>();
		const result = await setupMcp(
			{ broken: { command: "definitely-not-a-real-binary-xyz", args: [], timeoutMs: 2000 } },
			existing,
		);
		assert.deepEqual(result.tools, []);
		await result.cleanup();
	});
});


describe("Parallel Search opt-in", () => {
	it("does not connect by default", async () => {
		let calls = 0;
		const result = await setupMcp(undefined, new Set(), { connect: async () => { calls++; return null; } });
		assert.equal(calls, 0);
		assert.deepEqual(result.tools, []);
	});

	it("connects and discovers tools only when explicitly enabled", async () => {
		let received: any;
		const client = { listTools: async () => ({ tools: [{ name: "web_search", inputSchema: {} }] }), callTool: async () => ({ content: [] }) };
		const result = await setupMcp(undefined, new Set(), { parallelSearch: true, connect: async (name, config) => {
			received = { name, config };
			return { name, client: client as any, close: async () => {} };
		} });

		assert.deepEqual(received, { name: "parallel-search", config: { type: "http", url: "https://search.parallel.ai/mcp" } });
		assert.deepEqual(result.tools.map((tool) => tool.name), ["parallel-search__web_search"]);
		await result.cleanup();
	});

	it("preserves an existing server with the reserved name", async () => {
		let received: any;
		await setupMcp({ "parallel-search": { type: "http", url: "https://user.example/mcp", headers: { "X-Test": "kept" } } }, new Set(), { parallelSearch: true, connect: async (name, config) => { received = { name, config }; return null; } });

		assert.equal(received.config.url, "https://user.example/mcp");
		assert.deepEqual(received.config.headers, { "X-Test": "kept" });
	});
});
