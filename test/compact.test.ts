import { describe, it, before } from "node:test";
import assert from "node:assert/strict";

let compact: typeof import("../dist/compact.js");

before(async () => {
	compact = await import("../dist/compact.js");
});

describe("estimateTokens", () => {
	it("uses the ~4 chars per token heuristic (rounding up)", () => {
		assert.equal(compact.estimateTokens(""), 0);
		assert.equal(compact.estimateTokens("abcd"), 1);
		assert.equal(compact.estimateTokens("abcde"), 2);
	});
});

describe("estimateMessageTokens", () => {
	it("sums across message types and adds overhead for tool_use", () => {
		const msgs = [
			{ type: "user", content: "aaaa" },            // 1
			{ type: "assistant", content: "bbbb", thinking: "cccc" }, // 1 + 1
			{ type: "tool_use", toolName: "x", toolCallId: "1", args: {} }, // "{}" → 1 + 50
			{ type: "tool_result", toolName: "x", toolCallId: "1", content: "dddd", isError: false }, // 1
		] as any;
		// 1 + 2 + 51 + 1
		assert.equal(compact.estimateMessageTokens(msgs), 55);
	});

	it("returns 0 for an empty conversation", () => {
		assert.equal(compact.estimateMessageTokens([]), 0);
	});
});

describe("needsCompaction", () => {
	it("flags when estimate exceeds 75% of the context window", () => {
		const big = [{ type: "user", content: "x".repeat(4 * 800) }] as any; // ~800 tokens
		const r = compact.needsCompaction(big, 1000);
		assert.equal(r.needed, true);
		assert.equal(r.tokenEstimate, 800);
		assert.equal(r.ratio, 0.8);
	});

	it("does not flag when comfortably under the limit", () => {
		const small = [{ type: "user", content: "hello" }] as any;
		const r = compact.needsCompaction(small, 200000);
		assert.equal(r.needed, false);
		assert.ok(r.ratio < 0.75);
	});
});

describe("truncateToolResults", () => {
	it("truncates oversized tool results, keeping head and tail", () => {
		const content = "A".repeat(6000) + "B".repeat(6000); // 12000 chars
		const out = compact.truncateToolResults(
			[{ type: "tool_result", toolName: "t", toolCallId: "1", content, isError: false }] as any,
			10000,
		);
		const text = (out[0] as any).content;
		assert.ok(text.length < content.length);
		assert.ok(text.startsWith("A"));
		assert.ok(text.endsWith("B"));
		assert.match(text, /chars truncated/);
	});

	it("leaves small results and non-tool messages untouched", () => {
		const msgs = [
			{ type: "tool_result", toolName: "t", toolCallId: "1", content: "short", isError: false },
			{ type: "user", content: "x".repeat(50000) },
		] as any;
		const out = compact.truncateToolResults(msgs, 100);
		assert.equal((out[0] as any).content, "short");
		assert.equal((out[1] as any).content.length, 50000, "user message is never truncated");
	});
});

describe("messagesToText / buildCompactPrompt", () => {
	it("renders substantive turns and drops deltas/system noise", () => {
		const msgs = [
			{ type: "system", subtype: "session_start", content: "started" },
			{ type: "delta", deltaType: "text", content: "streaming..." },
			{ type: "user", content: "hi" },
			{ type: "assistant", content: "hello" },
			{ type: "tool_use", toolName: "read", toolCallId: "1", args: { path: "a" } },
			{ type: "tool_result", toolName: "read", toolCallId: "1", content: "file body", isError: false },
		] as any;
		const text = compact.messagesToText(msgs);
		assert.match(text, /User: hi/);
		assert.match(text, /Assistant: hello/);
		assert.match(text, /Tool call: read/);
		assert.match(text, /Tool result \[read\]/);
		assert.doesNotMatch(text, /streaming/);
		assert.doesNotMatch(text, /started/);
	});

	it("buildCompactPrompt wraps the transcript with instructions", () => {
		const prompt = compact.buildCompactPrompt([{ type: "user", content: "hi" }] as any);
		assert.match(prompt, /Summarize this conversation/);
		assert.match(prompt, /User: hi/);
	});

	it("buildCompactPrompt returns empty string for an empty conversation", () => {
		assert.equal(compact.buildCompactPrompt([]), "");
	});
});
