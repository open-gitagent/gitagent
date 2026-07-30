import { describe, it, before } from "node:test";
import assert from "node:assert/strict";

let CostTracker: typeof import("../dist/cost-tracker.js").CostTracker;

before(async () => {
	({ CostTracker } = await import("../dist/cost-tracker.js"));
});

const usage = (over: Partial<Record<string, number>> = {}) => ({
	inputTokens: 100,
	outputTokens: 40,
	cacheReadTokens: 0,
	cacheWriteTokens: 0,
	totalTokens: 140,
	costUsd: 0.5,
	...over,
});

describe("CostTracker", () => {
	it("starts empty", () => {
		const c = new CostTracker().get();
		assert.equal(c.totalInputTokens, 0);
		assert.equal(c.totalOutputTokens, 0);
		assert.equal(c.totalCostUsd, 0);
		assert.equal(c.totalRequests, 0);
		assert.deepEqual(c.modelUsage, {});
		assert.ok(c.startTime > 0);
	});

	it("accumulates session totals across requests", () => {
		const t = new CostTracker();
		t.add("openai:gpt-4o", usage());
		t.add("openai:gpt-4o", usage({ inputTokens: 200, outputTokens: 60, costUsd: 1 }));
		const c = t.get();
		assert.equal(c.totalInputTokens, 300);
		assert.equal(c.totalOutputTokens, 100);
		assert.equal(c.totalCostUsd, 1.5);
		assert.equal(c.totalRequests, 2);
	});

	it("aggregates usage per model key", () => {
		const t = new CostTracker();
		t.add("openai:gpt-4o", usage());
		t.add("anthropic:claude", usage({ inputTokens: 10, outputTokens: 5, costUsd: 0.1 }));
		t.add("openai:gpt-4o", usage({ inputTokens: 1, outputTokens: 1, costUsd: 0 }));
		const c = t.get();
		assert.equal(Object.keys(c.modelUsage).length, 2);
		assert.equal(c.modelUsage["openai:gpt-4o"].requests, 2);
		assert.equal(c.modelUsage["openai:gpt-4o"].inputTokens, 101);
		assert.equal(c.modelUsage["anthropic:claude"].requests, 1);
		assert.equal(c.modelUsage["anthropic:claude"].inputTokens, 10);
	});

	it("derives totalTokens per model when not supplied", () => {
		const t = new CostTracker();
		// Omit totalTokens → falls back to input + output.
		t.add("m", { inputTokens: 7, outputTokens: 3 } as any);
		assert.equal(t.get().modelUsage["m"].totalTokens, 10);
	});

	it("get() returns a defensive copy of modelUsage", () => {
		const t = new CostTracker();
		t.add("m", usage());
		const snapshot = t.get();
		delete (snapshot.modelUsage as any)["m"];
		// Mutating the snapshot must not corrupt tracker state.
		assert.ok(t.get().modelUsage["m"], "internal modelUsage survived external mutation");
	});

	it("reset() clears all counters", () => {
		const t = new CostTracker();
		t.add("m", usage());
		t.reset();
		const c = t.get();
		assert.equal(c.totalInputTokens, 0);
		assert.equal(c.totalRequests, 0);
		assert.deepEqual(c.modelUsage, {});
	});
});
