import { describe, it, before } from "node:test";
import assert from "node:assert/strict";

let createLoopGuard: typeof import("../dist/loop-guard.js").createLoopGuard;

before(async () => {
	({ createLoopGuard } = await import("../dist/loop-guard.js"));
});

// Build a minimal AfterToolCallContext. The guard only reads assistantMessage
// (by reference), toolCall.name, args, and result.content.
function ctx(turn: object, name: string, args: unknown, details: unknown = undefined) {
	return {
		assistantMessage: turn as any,
		toolCall: { type: "toolCall", id: "x", name, arguments: args as any },
		args,
		result: { content: [{ type: "text", text: "ok" }], details },
		isError: false,
		context: {} as any,
	};
}

describe("createLoopGuard", () => {
	it("does not terminate under normal, varied tool use", async () => {
		const guard = createLoopGuard({ maxTurns: 50 });
		for (let i = 0; i < 10; i++) {
			const r = await guard.afterToolCall(ctx({}, "read", { path: `f${i}` }));
			assert.equal(r, undefined);
		}
	});

	it("terminates when the same tool call repeats maxRepeats times", async () => {
		const guard = createLoopGuard({ maxTurns: 1000, maxRepeats: 5 });
		let terminatedAt = -1;
		for (let i = 0; i < 10; i++) {
			// Distinct turn each time, but identical name+args → stuck loop
			const r = await guard.afterToolCall(ctx({}, "write", { path: "a", content: "" }));
			if (r?.terminate) {
				terminatedAt = i;
				break;
			}
		}
		assert.equal(terminatedAt, 4, "should terminate on the 5th identical call");
	});

	it("resets the repeat counter when arguments change", async () => {
		const guard = createLoopGuard({ maxTurns: 1000, maxRepeats: 3 });
		// 2 identical, then a different one, then 2 identical again → never 3 in a row
		assert.equal((await guard.afterToolCall(ctx({}, "write", { p: 1 })))?.terminate, undefined);
		assert.equal((await guard.afterToolCall(ctx({}, "write", { p: 1 })))?.terminate, undefined);
		assert.equal((await guard.afterToolCall(ctx({}, "write", { p: 2 })))?.terminate, undefined);
		assert.equal((await guard.afterToolCall(ctx({}, "write", { p: 2 })))?.terminate, undefined);
	});

	it("terminates when maxTurns distinct turns is exceeded", async () => {
		const guard = createLoopGuard({ maxTurns: 3, maxRepeats: 100 });
		// Each call is a new turn object with varied args (no repeat trip).
		assert.equal((await guard.afterToolCall(ctx({}, "read", { p: 1 })))?.terminate, undefined);
		assert.equal((await guard.afterToolCall(ctx({}, "read", { p: 2 })))?.terminate, undefined);
		const third = await guard.afterToolCall(ctx({}, "read", { p: 3 }));
		assert.equal(third?.terminate, true, "3rd distinct turn hits maxTurns=3");
	});

	it("counts one turn per batch (shared assistantMessage reference)", async () => {
		const guard = createLoopGuard({ maxTurns: 2, maxRepeats: 100 });
		const turnA = {};
		// Two calls in the SAME batch → one turn, must not trip maxTurns=2 yet.
		assert.equal((await guard.afterToolCall(ctx(turnA, "read", { p: 1 })))?.terminate, undefined);
		assert.equal((await guard.afterToolCall(ctx(turnA, "read", { p: 2 })))?.terminate, undefined);
		// New batch → second turn → hits maxTurns=2.
		const turnB = {};
		const r = await guard.afterToolCall(ctx(turnB, "read", { p: 3 }));
		assert.equal(r?.terminate, true);
	});

	it("preserves original tool output (content + details) when terminating", async () => {
		const guard = createLoopGuard({ maxTurns: 1, maxRepeats: 100 });
		const r = await guard.afterToolCall(ctx({}, "read", { p: 1 }, { rows: 3 }));
		assert.equal(r?.terminate, true);
		assert.equal(r?.content?.[0]?.text, "ok");
		assert.match(r?.content?.[1]?.text ?? "", /loop-guard/);
		assert.deepEqual(r?.details, { rows: 3 });
	});
});
