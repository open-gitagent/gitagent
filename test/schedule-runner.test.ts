import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { MAX_TIMEOUT_MS, splitTimeoutDelay } from "../dist/schedule-runner.js";

describe("schedule timer safety", () => {
	it("keeps ordinary delays in one timer", () => {
		assert.deepEqual(splitTimeoutDelay(60_000), [60_000]);
	});

	it("splits far-future delays at Node's timer limit", () => {
		assert.deepEqual(splitTimeoutDelay(MAX_TIMEOUT_MS + 5_000), [MAX_TIMEOUT_MS, 5_000]);
	});

	it("normalizes negative and fractional delays", () => {
		assert.deepEqual(splitTimeoutDelay(-1), [0]);
		assert.deepEqual(splitTimeoutDelay(1_500.9), [1_500]);
	});
});
