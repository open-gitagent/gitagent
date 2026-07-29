import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

// Dynamic imports since the project is ESM
let query: typeof import("../dist/exports.js").query;
let tool: typeof import("../dist/exports.js").tool;
let loadAgent: typeof import("../dist/exports.js").loadAgent;
let buildTypeboxSchema: typeof import("../dist/tool-loader.js").buildTypeboxSchema;
let wrapToolWithProgrammaticHooks: typeof import("../dist/sdk-hooks.js").wrapToolWithProgrammaticHooks;

before(async () => {
	const exports = await import("../dist/exports.js");
	query = exports.query;
	tool = exports.tool;
	loadAgent = exports.loadAgent;
	const toolLoader = await import("../dist/tool-loader.js");
	buildTypeboxSchema = toolLoader.buildTypeboxSchema;
	const sdkHooks = await import("../dist/sdk-hooks.js");
	wrapToolWithProgrammaticHooks = sdkHooks.wrapToolWithProgrammaticHooks;
});

// ── Exports ────────────────────────────────────────────────────────────

describe("exports", () => {
	it("exports query and tool functions", async () => {
		const mod = await import("../dist/exports.js");
		assert.equal(typeof mod.query, "function");
		assert.equal(typeof mod.tool, "function");
		assert.equal(typeof mod.loadAgent, "function");
	});
});

// ── tool() helper ──────────────────────────────────────────────────────

describe("tool()", () => {
	it("creates a GCToolDefinition with correct fields", () => {
		const handler = async (args: any) => `result: ${args.q}`;
		const t = tool("search", "Search things", {
			properties: { q: { type: "string", description: "Query" } },
			required: ["q"],
		}, handler);

		assert.equal(t.name, "search");
		assert.equal(t.description, "Search things");
		assert.deepEqual(t.inputSchema.required, ["q"]);
		assert.equal(t.handler, handler);
	});

	it("handler returns string", async () => {
		const t = tool("echo", "Echo input", {
			properties: { text: { type: "string" } },
		}, async (args) => args.text);

		const result = await t.handler({ text: "hello" });
		assert.equal(result, "hello");
	});

	it("handler returns object with text and details", async () => {
		const t = tool("rich", "Rich output", {
			properties: {},
		}, async () => ({ text: "done", details: { count: 42 } }));

		const result = await t.handler({});
		assert.deepEqual(result, { text: "done", details: { count: 42 } });
	});
});

// ── buildTypeboxSchema ─────────────────────────────────────────────────

describe("buildTypeboxSchema()", () => {
	it("converts string property", () => {
		const schema = buildTypeboxSchema({
			properties: { name: { type: "string", description: "A name" } },
			required: ["name"],
		});
		assert.equal(schema.type, "object");
		assert.ok(schema.properties.name);
	});

	it("converts number property", () => {
		const schema = buildTypeboxSchema({
			properties: { count: { type: "number", description: "Count" } },
			required: ["count"],
		});
		assert.ok(schema.properties.count);
	});

	it("converts boolean property", () => {
		const schema = buildTypeboxSchema({
			properties: { flag: { type: "boolean", description: "Flag" } },
		});
		assert.ok(schema.properties.flag);
	});

	it("converts array property", () => {
		const schema = buildTypeboxSchema({
			properties: { items: { type: "array", description: "Items" } },
		});
		assert.ok(schema.properties.items);
	});

	it("handles empty schema", () => {
		const schema = buildTypeboxSchema({});
		assert.equal(schema.type, "object");
	});

	it("marks non-required fields as optional", () => {
		const schema = buildTypeboxSchema({
			properties: {
				required_field: { type: "string" },
				optional_field: { type: "string" },
			},
			required: ["required_field"],
		});
		// Typebox Optional wraps with a modifier
		assert.ok(schema.properties.required_field);
		assert.ok(schema.properties.optional_field);
	});
});

// ── wrapToolWithProgrammaticHooks ──────────────────────────────────────

describe("wrapToolWithProgrammaticHooks()", () => {
	function makeMockTool(name: string = "test_tool") {
		return {
			name,
			label: name,
			description: "A test tool",
			parameters: buildTypeboxSchema({ properties: { x: { type: "string" } } }),
			execute: async (_id: string, args: any) => ({
				content: [{ type: "text" as const, text: `executed with ${JSON.stringify(args)}` }],
				details: undefined,
			}),
		};
	}

	it("returns tool unchanged when no preToolUse hook", () => {
		const t = makeMockTool();
		const wrapped = wrapToolWithProgrammaticHooks(t, {}, "sess-1", "agent");
		assert.equal(wrapped, t);
	});

	it("allows execution when hook returns allow", async () => {
		const t = makeMockTool();
		const wrapped = wrapToolWithProgrammaticHooks(t, {
			preToolUse: async () => ({ action: "allow" }),
		}, "sess-1", "agent");

		const result = await wrapped.execute("call-1", { x: "hello" });
		assert.ok(result.content[0].text.includes("hello"));
	});

	it("blocks execution when hook returns block", async () => {
		const t = makeMockTool();
		const wrapped = wrapToolWithProgrammaticHooks(t, {
			preToolUse: async () => ({ action: "block", reason: "not allowed" }),
		}, "sess-1", "agent");

		await assert.rejects(
			() => wrapped.execute("call-1", { x: "hello" }),
			(err: Error) => {
				assert.ok(err.message.includes("blocked by hook"));
				assert.ok(err.message.includes("not allowed"));
				return true;
			},
		);
	});

	it("modifies args when hook returns modify", async () => {
		const t = makeMockTool();
		const wrapped = wrapToolWithProgrammaticHooks(t, {
			preToolUse: async () => ({
				action: "modify",
				args: { x: "modified" },
			}),
		}, "sess-1", "agent");

		const result = await wrapped.execute("call-1", { x: "original" });
		assert.ok(result.content[0].text.includes("modified"));
		assert.ok(!result.content[0].text.includes("original"));
	});

	it("passes correct context to hook", async () => {
		const t = makeMockTool("my_tool");
		let captured: any = null;

		const wrapped = wrapToolWithProgrammaticHooks(t, {
			preToolUse: async (ctx) => {
				captured = ctx;
				return { action: "allow" };
			},
		}, "sess-42", "my_agent");

		await wrapped.execute("call-1", { x: "test" });

		assert.equal(captured.sessionId, "sess-42");
		assert.equal(captured.agentName, "my_agent");
		assert.equal(captured.event, "PreToolUse");
		assert.equal(captured.toolName, "my_tool");
		assert.deepEqual(captured.args, { x: "test" });
	});
});

// ── query() error handling ─────────────────────────────────────────────

describe("query()", () => {
	it("reports invalid timeout values without loading an agent", async () => {
		const messages: any[] = [];
		for await (const msg of query({
			prompt: "hello",
			dir: "/nonexistent/path",
			timeoutMs: 0,
		})) {
			messages.push(msg);
		}

		const errorMsg = messages.find((m) => m.type === "system" && m.subtype === "error");
		assert.match(errorMsg?.content ?? "", /timeoutMs must be a finite number greater than zero/);
	});

	it("honors an already-aborted controller before starting the model", async () => {
		const agentDir = mkdtempSync(join(tmpdir(), "gitagent-abort-"));
		try {
			writeFileSync(join(agentDir, "agent.yaml"), `spec_version: "1.0"\nname: test\nversion: "1.0.0"\ndescription: test\nmodel:\n  preferred: openai:gpt-4o-mini\n  fallback: []\ntools: []\nruntime:\n  max_turns: 1\n`);
			const controller = new AbortController();
			controller.abort();
			const messages: any[] = [];
			const drain = (async () => {
				for await (const msg of query({
					prompt: "hello",
					dir: agentDir,
					abortController: controller,
					timeoutMs: 100,
				})) {
					messages.push(msg);
				}
			})();
			let deadlineTimer: ReturnType<typeof setTimeout>;
			const deadline = new Promise((_, reject) => {
				deadlineTimer = setTimeout(
					() => reject(new Error("aborted query did not settle")),
					2000,
				);
			});
			try {
				await Promise.race([drain, deadline]);
			} finally {
				clearTimeout(deadlineTimer!);
			}
			assert.ok(messages.some((m) => m.type === "system"));
		} finally {
			rmSync(agentDir, { recursive: true, force: true });
		}
	});

	it("emits error system message when agent dir is invalid", async () => {
		const messages: any[] = [];
		for await (const msg of query({
			prompt: "hello",
			dir: "/nonexistent/path/to/agent",
		})) {
			messages.push(msg);
		}

		assert.ok(messages.length > 0);
		const errorMsg = messages.find((m) => m.type === "system" && m.subtype === "error");
		assert.ok(errorMsg, "should have an error system message");
	});

	it("returns Query object with expected methods", () => {
		const q = query({
			prompt: "hello",
			dir: "/nonexistent",
		});

		assert.equal(typeof q.abort, "function");
		assert.equal(typeof q.steer, "function");
		assert.equal(typeof q.sessionId, "function");
		assert.equal(typeof q.messages, "function");
		assert.equal(typeof q.next, "function");
		assert.equal(typeof q[Symbol.asyncIterator], "function");

		// Clean up - drain the generator
		q.return();
	});

	it("fires onError hook on failure", async () => {
		let errorCaptured: string | null = null;

		const messages: any[] = [];
		for await (const msg of query({
			prompt: "hello",
			dir: "/nonexistent/path",
			hooks: {
				onError: async (ctx) => {
					errorCaptured = ctx.error;
				},
			},
		})) {
			messages.push(msg);
		}

		// Give the async hook a moment to fire
		await new Promise((r) => setTimeout(r, 50));
		assert.ok(errorCaptured, "onError hook should have been called");
	});

	it("messages() collects emitted messages", async () => {
		const q = query({
			prompt: "hello",
			dir: "/nonexistent/path",
		});

		for await (const _msg of q) {
			// drain
		}

		const collected = q.messages();
		assert.ok(Array.isArray(collected));
		assert.ok(collected.length > 0);
	});
});
