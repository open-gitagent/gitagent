import { describe, it, before } from "node:test";
import assert from "node:assert/strict";

let computeCostUsd: typeof import("../dist/cost-tracker.js").computeCostUsd;
let CostTracker: typeof import("../dist/cost-tracker.js").CostTracker;

before(async () => {
	({ computeCostUsd, CostTracker } = await import("../dist/cost-tracker.js"));
});

// gpt-4o-style rates: $/1M tokens.
const RATES = { input: 2.5, output: 10, cacheRead: 1.25, cacheWrite: 0 };
const ZERO_RATES = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };

describe("computeCostUsd", () => {
	it("prices input + output from per-million rates", () => {
		// 1,000,000 input @ $2.5 + 1,000,000 output @ $10 = $12.5
		const c = computeCostUsd({ inputTokens: 1_000_000, outputTokens: 1_000_000 }, RATES);
		assert.equal(c, 12.5);
	});

	it("includes cache read/write pricing", () => {
		const c = computeCostUsd(
			{ inputTokens: 0, outputTokens: 0, cacheReadTokens: 2_000_000, cacheWriteTokens: 1_000_000 },
			{ input: 0, output: 0, cacheRead: 1.25, cacheWrite: 5 },
		);
		// 2M * 1.25 + 1M * 5 = 2.5 + 5 = 7.5
		assert.equal(c, 7.5);
	});

	it("returns null when tokens were used but the model has no pricing", () => {
		const c = computeCostUsd({ inputTokens: 500, outputTokens: 200 }, ZERO_RATES);
		assert.equal(c, null, "unknown cost, not a misleading $0");
	});

	it("returns 0 (not null) when there are no tokens and no rates", () => {
		assert.equal(computeCostUsd({ inputTokens: 0, outputTokens: 0 }, ZERO_RATES), 0);
	});

	it("computes even when only some rates are non-zero", () => {
		// cacheWrite rate is 0 for gpt-4o but input>0 means we still price it.
		const c = computeCostUsd({ inputTokens: 400_000, outputTokens: 0 }, RATES);
		assert.equal(c, 1); // 0.4M * 2.5 = 1.0
	});
});

describe("CostTracker.costDataAvailable", () => {
	it("defaults to true and stays true for priced requests", () => {
		const t = new CostTracker();
		assert.equal(t.get().costDataAvailable, true);
		t.add("openai:gpt-4o", { inputTokens: 100, outputTokens: 50, costUsd: 0.01 }, true);
		assert.equal(t.get().costDataAvailable, true);
	});

	it("flips to false once a request can't be priced", () => {
		const t = new CostTracker();
		t.add("custom:model", { inputTokens: 100, outputTokens: 50, costUsd: 0 }, false);
		assert.equal(t.get().costDataAvailable, false);
		// tokens still tracked even though cost is unknown
		assert.equal(t.get().totalInputTokens, 100);
	});

	it("reset() restores costDataAvailable to true", () => {
		const t = new CostTracker();
		t.add("custom:model", { inputTokens: 1, outputTokens: 1 }, false);
		t.reset();
		assert.equal(t.get().costDataAvailable, true);
	});
});
