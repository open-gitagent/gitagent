import { describe, it, before, beforeEach, afterEach, mock } from "node:test";
import assert from "node:assert/strict";

// Dynamic imports since the project is ESM and we test the built output.
let setupA2A: typeof import("../dist/a2a/manager.js").setupA2A;
let partsToText: typeof import("../dist/a2a/manager.js").partsToText;
let A2AClient: typeof import("@a2a-js/sdk/client").A2AClient;

before(async () => {
	const mgr = await import("../dist/a2a/manager.js");
	setupA2A = mgr.setupA2A;
	partsToText = mgr.partsToText;
	A2AClient = (await import("@a2a-js/sdk/client")).A2AClient;
});

// ── Test doubles ─────────────────────────────────────────────────────────

function fakeCard(skills: any[], streaming = false): any {
	return {
		protocolVersion: "0.3.0",
		name: "Research",
		description: "A research agent",
		url: "http://example.test",
		version: "1.0.0",
		capabilities: { streaming },
		defaultInputModes: ["text/plain"],
		defaultOutputModes: ["text/plain"],
		skills,
	};
}

function textMessage(text: string): any {
	return { kind: "message", messageId: "m1", role: "agent", parts: [{ kind: "text", text }] };
}

function installFakeClient(client: any) {
	mock.method(A2AClient, "fromCardUrl", async () => client);
}

afterEach(() => mock.restoreAll());

// Silence the manager's console output during tests.
beforeEach(() => {
	mock.method(console, "log", () => {});
	mock.method(console, "warn", () => {});
});

// ── partsToText ──────────────────────────────────────────────────────────

describe("partsToText", () => {
	it("joins text parts", () => {
		const out = partsToText([
			{ kind: "text", text: "hello" },
			{ kind: "text", text: "world" },
		] as any);
		assert.equal(out, "hello\nworld");
	});

	it("summarizes file parts instead of inlining bytes", () => {
		const out = partsToText([{ kind: "file", file: { name: "report.pdf" } }] as any);
		assert.match(out, /\[file: report\.pdf\]/);
	});

	it("renders data parts as JSON", () => {
		const out = partsToText([{ kind: "data", data: { a: 1 } }] as any);
		assert.match(out, /"a": 1/);
	});

	it("returns empty string for non-array", () => {
		assert.equal(partsToText(undefined), "");
	});
});

// ── setupA2A: discovery & mapping ─────────────────────────────────────────

describe("setupA2A", () => {
	it("returns no tools and a no-op cleanup when nothing is configured", async () => {
		const r1 = await setupA2A(undefined, new Set());
		const r2 = await setupA2A({}, new Set());
		assert.equal(r1.tools.length, 0);
		assert.equal(r2.tools.length, 0);
		await r1.cleanup(); // does not throw
	});

	it("maps one tool per skill, namespaced <agent>__<skill>", async () => {
		installFakeClient({
			getAgentCard: async () =>
				fakeCard([
					{ id: "web_search", name: "Web Search", description: "Search", tags: [] },
					{ id: "fact_check", name: "Fact Check", description: "Check", tags: [] },
				]),
		});
		const { tools } = await setupA2A(
			{ research: { url: "http://example.test" } },
			new Set(),
		);
		const names = tools.map((t) => t.name).sort();
		assert.deepEqual(names, ["research__fact_check", "research__web_search"]);
	});

	it("falls back to a single agent-named tool when no skills are declared", async () => {
		installFakeClient({ getAgentCard: async () => fakeCard([]) });
		const { tools } = await setupA2A(
			{ helper: { url: "http://example.test" } },
			new Set(),
		);
		assert.equal(tools.length, 1);
		assert.equal(tools[0].name, "helper");
	});

	it("skips tools that collide with existing tool names", async () => {
		installFakeClient({
			getAgentCard: async () =>
				fakeCard([{ id: "read", name: "Read", description: "x", tags: [] }]),
		});
		const { tools } = await setupA2A(
			{ fs: { url: "http://example.test" } },
			new Set(["fs__read"]),
		);
		assert.equal(tools.length, 0);
	});

	it("skips an agent with a missing url but keeps others", async () => {
		installFakeClient({
			getAgentCard: async () =>
				fakeCard([{ id: "go", name: "Go", description: "x", tags: [] }]),
		});
		const { tools } = await setupA2A(
			{ bad: {} as any, good: { url: "http://example.test" } },
			new Set(),
		);
		assert.deepEqual(tools.map((t) => t.name), ["good__go"]);
	});

	it("is non-fatal when an agent connection fails", async () => {
		mock.method(A2AClient, "fromCardUrl", async () => {
			throw new Error("ECONNREFUSED");
		});
		const { tools } = await setupA2A(
			{ down: { url: "http://example.test", timeoutMs: 200 } },
			new Set(),
		);
		assert.equal(tools.length, 0); // skipped, no throw
	});
});

// ── tool execution ────────────────────────────────────────────────────────

describe("A2A tool execution", () => {
	it("blocking call returns the remote agent's text", async () => {
		installFakeClient({
			getAgentCard: async () => fakeCard([{ id: "ask", name: "Ask", description: "x", tags: [] }], false),
			sendMessage: async () => ({ result: textMessage("the answer is 42") }),
		});
		const { tools } = await setupA2A({ q: { url: "http://example.test" } }, new Set());
		const res = await tools[0].execute("call1", { message: "what is the answer?" });
		assert.equal(res.content[0].text, "the answer is 42");
	});

	it("propagates a remote error into the tool result instead of throwing", async () => {
		installFakeClient({
			getAgentCard: async () => fakeCard([{ id: "ask", name: "Ask", description: "x", tags: [] }], false),
			sendMessage: async () => ({ error: { code: -32000, message: "boom" } }),
		});
		const { tools } = await setupA2A({ q: { url: "http://example.test" } }, new Set());
		const res = await tools[0].execute("call1", { message: "hi" });
		assert.match(res.content[0].text, /failed: boom/);
	});

	it("rejects an empty message", async () => {
		installFakeClient({
			getAgentCard: async () => fakeCard([{ id: "ask", name: "Ask", description: "x", tags: [] }], false),
		});
		const { tools } = await setupA2A({ q: { url: "http://example.test" } }, new Set());
		const res = await tools[0].execute("call1", { message: "   " });
		assert.match(res.content[0].text, /required/);
	});

	it("streaming call accumulates events and pushes partial updates", async () => {
		async function* stream() {
			yield { kind: "status-update", status: { state: "working", message: textMessage("partial") }, final: false };
			yield { kind: "artifact-update", artifact: { artifactId: "a1", parts: [{ kind: "text", text: "final answer" }] } };
			yield { kind: "status-update", status: { state: "completed" }, final: true };
		}
		installFakeClient({
			getAgentCard: async () => fakeCard([{ id: "ask", name: "Ask", description: "x", tags: [] }], true),
			sendMessageStream: () => stream(),
		});
		const { tools } = await setupA2A({ q: { url: "http://example.test" } }, new Set());
		const updates: string[] = [];
		const res = await tools[0].execute("call1", { message: "go" }, undefined, (p: any) =>
			updates.push(p.content[0].text),
		);
		assert.match(res.content[0].text, /partial/);
		assert.match(res.content[0].text, /final answer/);
		assert.ok(updates.length >= 1, "expected at least one partial update");
	});
});
