import { describe, it, before } from "node:test";
import assert from "node:assert/strict";

let scheduleAt: typeof import("../dist/schedule-runner.js").scheduleAt;
let activeTimers: typeof import("../dist/schedule-runner.js").activeTimers;

before(async () => {
	const mod = await import("../dist/schedule-runner.js");
	scheduleAt = mod.scheduleAt;
	activeTimers = mod.activeTimers;
});

const MAX_TIMEOUT_MS = 2_147_483_647;

describe("scheduleAt (timer overflow guard)", () => {
	it("delay < MAX_TIMEOUT_MS: fires in a single setTimeout, callback called exactly once", (t) => {
		t.mock.timers.enable({ apis: ["setTimeout", "Date"] });

		const start = Date.now();
		const delay = 10 * 24 * 60 * 60 * 1000; // 10 days — well under the limit
		const target = start + delay;

		let fireCount = 0;
		scheduleAt("short-job", target, () => fireCount++);

		assert.equal(fireCount, 0, "must not fire before the delay elapses");
		t.mock.timers.tick(delay);
		assert.equal(fireCount, 1, "must fire exactly once once the delay elapses");
	});

	it("delay slightly above MAX_TIMEOUT_MS: fires via two chunks, callback called once, at the right time", (t) => {
		t.mock.timers.enable({ apis: ["setTimeout", "Date"] });

		const start = Date.now();
		const delay = MAX_TIMEOUT_MS + 1000; // just over the limit — needs a 2nd chunk
		const target = start + delay;

		let fireCount = 0;
		let firedAt = 0;
		scheduleAt("two-chunk-job", target, () => {
			fireCount++;
			firedAt = Date.now();
		});

		// After the first chunk (exactly MAX_TIMEOUT_MS), it must not have fired yet —
		// there's still 1000ms of real delay left.
		t.mock.timers.tick(MAX_TIMEOUT_MS);
		assert.equal(fireCount, 0, "must not fire after only the first chunk");

		// The remaining 1000ms is the second chunk.
		t.mock.timers.tick(1000);
		assert.equal(fireCount, 1, "must fire exactly once after the second chunk");
		assert.equal(firedAt, target, "must fire exactly at the real target time");
	});

	it("delay <= 0 at call time: callback fires synchronously, no timer scheduled", (t) => {
		t.mock.timers.enable({ apis: ["setTimeout", "Date"] });

		const now = Date.now();
		let fired = false;
		scheduleAt("past-job", now - 1000, () => {
			fired = true;
		});

		assert.equal(fired, true, "must fire immediately when the target time is already in the past");
		assert.equal(activeTimers.has("past-job"), false, "must not leave a dangling timer entry");
	});

	it("cancel mid-chunk: clearing the activeTimers entry prevents the callback from firing", (t) => {
		t.mock.timers.enable({ apis: ["setTimeout", "Date"] });

		const start = Date.now();
		const delay = MAX_TIMEOUT_MS + 1000; // needs 2 chunks, so there's a "mid-chunk" to cancel during
		const target = start + delay;

		let fired = false;
		scheduleAt("cancelled-job", target, () => {
			fired = true;
		});

		// Cancel after the first chunk has elapsed, before the second chunk's callback fires.
		t.mock.timers.tick(MAX_TIMEOUT_MS);
		const timer = activeTimers.get("cancelled-job");
		assert.ok(timer, "a pending timer must exist mid-chunk");
		clearTimeout(timer);
		activeTimers.delete("cancelled-job");

		// Advance past the point where it would have fired, if not cancelled.
		t.mock.timers.tick(1000);
		assert.equal(fired, false, "must not fire after being cancelled mid-chunk");
	});
});
