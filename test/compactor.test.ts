import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import type { AgentMessage } from "@mariozechner/pi-agent-core";
import type { AssistantMessage, ToolResultMessage, UserMessage } from "@mariozechner/pi-ai";

let Compactor: typeof import("../dist/compactor.js").Compactor;
let createTransformContext: typeof import("../dist/compactor.js").createTransformContext;

before(async () => {
	const mod = await import("../dist/compactor.js");
	Compactor = mod.Compactor;
	createTransformContext = mod.createTransformContext;
});

function user(text: string): UserMessage {
	return { role: "user", content: text, timestamp: Date.now() };
}

function toolResult(id: string, name: string, content: string): ToolResultMessage {
	return {
		role: "toolResult",
		toolCallId: id,
		toolName: name,
		content: [{ type: "text", text: content }],
		isError: false,
		timestamp: Date.now(),
	};
}

function assistantWithToolCall(id: string, name: string, args: object = {}): AssistantMessage {
	return {
		role: "assistant",
		content: [{ type: "toolCall", id, name, arguments: args }],
		api: "openai-completions",
		provider: "test",
		model: "test",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "toolUse",
		timestamp: Date.now(),
	};
}

function assistantText(text: string): AssistantMessage {
	return {
		role: "assistant",
		content: [{ type: "text", text }],
		api: "openai-completions",
		provider: "test",
		model: "test",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "stop",
		timestamp: Date.now(),
	};
}

function toolText(m: AgentMessage): string {
	if ((m as ToolResultMessage).role !== "toolResult") return "";
	const c = (m as ToolResultMessage).content;
	return c.map((b) => (b.type === "text" ? b.text : "")).join("");
}

describe("Compactor.estimateTokens", () => {
	it("returns 0 for an empty transcript", () => {
		const c = new Compactor({ contextWindow: 128_000 });
		assert.equal(c.estimateTokens([]), 0);
	});

	it("counts user / tool / assistant content", () => {
		const c = new Compactor({ contextWindow: 128_000, charsPerToken: 4 });
		const msgs: AgentMessage[] = [
			user("abcd"), // 4 chars → 1
			toolResult("1", "read", "efgh"), // 4 + 32 → 9
		];
		assert.equal(c.estimateTokens(msgs), Math.ceil((4 + 36) / 4));
	});
});

describe("Compactor.apply (truncate-only)", () => {
	it("returns under-budget transcripts unchanged in length", () => {
		const c = new Compactor({ contextWindow: 128_000 });
		const msgs: AgentMessage[] = [user("hi")];
		const out = c.apply(msgs);
		assert.equal(out.length, 1);
		assert.equal((out[0] as UserMessage).content, "hi");
	});

	it("gets under budget without dropping messages", () => {
		const big = "x".repeat(100_000);
		const msgs: AgentMessage[] = [
			user("do the task"),
			toolResult("1", "read", big),
		];
		const c = new Compactor({ contextWindow: 2000 }); // budget ≈ 1500
		assert.ok(c.estimateTokens(msgs) > c.maxTokens);
		const out = c.apply(msgs);
		assert.ok(c.estimateTokens(out) <= c.maxTokens, "must be under budget");
		assert.equal(out.length, msgs.length, "never drop messages");
		assert.ok(toolText(out[1]).length < big.length);
		assert.match(toolText(out[1]), /omitted to fit the context window/);
	});

	it("truncates assistant text when tool truncation is not enough", () => {
		const c = new Compactor({ contextWindow: 800, summarize: false });
		const msgs: AgentMessage[] = [
			user("start"),
			assistantText("Y".repeat(50_000)),
		];
		assert.ok(c.estimateTokens(msgs) > c.maxTokens);
		const out = c.apply(msgs);
		assert.equal(out.length, 2);
		const text = ((out[1] as AssistantMessage).content[0] as { text: string }).text;
		assert.ok(text.length < 50_000);
		assert.match(text, /omitted/);
	});

	it("keeps head and tail of truncated tool results", () => {
		const content = "A".repeat(5000) + "B".repeat(5000);
		const c = new Compactor({ contextWindow: 500 });
		const out = c.apply([toolResult("1", "t", content)]);
		const text = toolText(out[0]);
		assert.ok(text.startsWith("A"));
		assert.ok(text.endsWith("B"));
	});
});

describe("Compactor.compact (summarize head)", () => {
	it("folds aged head into a summary and keeps the recent tail", async () => {
		const big = "x".repeat(20_000);
		const msgs: AgentMessage[] = [user("start the task")];
		for (let i = 0; i < 6; i++) {
			msgs.push(assistantWithToolCall(`t${i}`, "read", { path: "f" }));
			msgs.push(toolResult(`t${i}`, "read", big));
		}
		msgs.push(user("the most recent instruction"));

		const c = new Compactor({ contextWindow: 4000 });
		assert.ok(c.estimateTokens(msgs) > c.maxTokens);

		const out = await c.compact(msgs);
		assert.ok(c.estimateTokens(out) <= c.maxTokens, "compacted under budget");
		const first = out[0] as UserMessage;
		assert.equal(first.role, "user");
		assert.match(String(first.content), /Summary of the earlier conversation/);
		assert.ok(
			out.some(
				(m) =>
					(m as UserMessage).role === "user" &&
					String((m as UserMessage).content).includes("most recent"),
			),
			"recent tail kept verbatim",
		);
	});

	it("does not start the tail on an orphan toolResult", async () => {
		const big = "z".repeat(30_000);
		const msgs: AgentMessage[] = [
			user("old"),
			assistantWithToolCall("a", "read"),
			toolResult("a", "read", big),
			assistantWithToolCall("b", "read"),
			toolResult("b", "read", big),
			user("new"),
		];
		const c = new Compactor({ contextWindow: 3000 });
		const out = await c.compact(msgs);
		// After the summary message, the next message must not be a bare toolResult
		// that lost its toolCall (unless the whole transcript was truncate-only).
		if (out.length > 1 && String((out[0] as UserMessage).content).includes("Summary")) {
			assert.notEqual((out[1] as ToolResultMessage).role, "toolResult");
		}
	});

	it("uses truncate-only when summarize is false", async () => {
		const big = "x".repeat(80_000);
		const msgs: AgentMessage[] = [user("hi"), toolResult("1", "read", big)];
		const c = new Compactor({ contextWindow: 2000, summarize: false });
		const out = await c.compact(msgs);
		assert.equal(out.length, 2, "truncate path preserves length");
		assert.ok(!String((out[0] as UserMessage).content).includes("Summary"));
	});

	it("calls optional summarizer when provided", async () => {
		let called = 0;
		const big = "x".repeat(20_000);
		const msgs: AgentMessage[] = [user("old work")];
		for (let i = 0; i < 4; i++) {
			msgs.push(assistantWithToolCall(`t${i}`, "read"));
			msgs.push(toolResult(`t${i}`, "read", big));
		}
		msgs.push(user("recent"));

		const c = new Compactor({
			contextWindow: 3000,
			summarizer: async () => {
				called += 1;
				return "SUMMARY: earlier work done";
			},
		});
		const out = await c.compact(msgs);
		assert.ok(called >= 1);
		assert.match(String((out[0] as UserMessage).content), /SUMMARY: earlier work done/);
	});
});

describe("Compactor.observeUsage / reset", () => {
	it("floors the estimate with usage.input so compact still runs", async () => {
		const c = new Compactor({ contextWindow: 1000, summarize: false });
		const msgs: AgentMessage[] = [user("hi"), toolResult("1", "r", "x".repeat(5000))];
		assert.ok(c.estimateTokens(msgs) > c.maxTokens);
		c.observeUsage(50_000);
		assert.ok(c.effectiveTokens(msgs) >= 50_000);
		assert.equal(c.needsCompact(msgs), true);
		const out = await c.compact(msgs);
		assert.equal(out.length, 2);
		assert.ok(c.estimateTokens(out) <= c.maxTokens);
	});

	it("reset clears usage and running summary", async () => {
		const c = new Compactor({ contextWindow: 4000 });
		c.observeUsage(99_000);
		assert.equal(c.status().lastUsageInput, 99_000);
		c.reset();
		assert.equal(c.status().lastUsageInput, 0);
		assert.equal(c.status().folded, 0);
	});
});

describe("createTransformContext", () => {
	it("never throws — returns original messages on failure", async () => {
		const c = new Compactor({ contextWindow: 100 });
		const boom = Object.create(c) as InstanceType<typeof Compactor>;
		boom.compact = async () => {
			throw new Error("boom");
		};
		const transform = createTransformContext(boom);
		const msgs: AgentMessage[] = [user("keep me")];
		const out = await transform(msgs);
		assert.equal(out, msgs);
	});

	it("returns compacted messages on success", async () => {
		const c = new Compactor({ contextWindow: 2000, summarize: false });
		const transform = createTransformContext(c);
		const msgs: AgentMessage[] = [user("task"), toolResult("1", "read", "x".repeat(100_000))];
		const out = await transform(msgs);
		assert.ok(c.estimateTokens(out) <= c.maxTokens);
	});
});
