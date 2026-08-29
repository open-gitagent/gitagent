// Tests for the core SkillFlow loop: step ordering, context threading, {input}
// substitution, approval-gate outcomes (approved / denied / no handler), and the
// progress event stream. `runStep` is injected, so the loop is exercised for real
// without spending an LLM call per assertion.

import test from "node:test";
import assert from "node:assert/strict";

import { executeFlow, APPROVAL_GATE, type FlowEvent, type FlowRunDeps } from "../src/flow-runner.ts";
import type { SkillFlowDefinition } from "../src/workflows.ts";

function flow(...steps: SkillFlowDefinition["steps"]): SkillFlowDefinition {
	return { name: "test-flow", description: "", steps };
}

/** Records every prompt runStep received and echoes a canned output per step. */
function recorder(outputs: string[] = []) {
	const prompts: string[] = [];
	const seen: { skill: string; index: number }[] = [];
	const deps: FlowRunDeps = {
		runStep: async (prompt, step, index) => {
			prompts.push(prompt);
			seen.push({ skill: step.skill, index });
			return outputs[index] ?? `output-${index}`;
		},
	};
	return { prompts, seen, deps };
}

// ── Ordering and context threading ─────────────────────────────────────

test("runs every step in order and returns their outputs", async () => {
	const { deps } = recorder();
	const r = await executeFlow(
		flow(
			{ skill: "one", prompt: "do one" },
			{ skill: "two", prompt: "do two" },
			{ skill: "three", prompt: "do three" },
		),
		"user input",
		deps,
	);

	assert.equal(r.completed, true);
	assert.deepEqual(r.steps.map((s) => s.skill), ["one", "two", "three"]);
	assert.deepEqual(r.steps.map((s) => s.output), ["output-0", "output-1", "output-2"]);
});

test("each step's prompt carries the previous steps' output forward", async () => {
	const { prompts, deps } = recorder(["FIRST-RESULT", "SECOND-RESULT"]);
	await executeFlow(
		flow({ skill: "one", prompt: "do one" }, { skill: "two", prompt: "do two" }),
		"seed",
		deps,
	);

	// Step 1 only sees the user's input.
	assert.match(prompts[0], /Context from previous steps:\nseed/);
	assert.doesNotMatch(prompts[0], /FIRST-RESULT/);

	// Step 2 sees step 1's labelled result appended.
	assert.match(prompts[1], /\[Step 1 result \(one\)\]: FIRST-RESULT/);
	assert.match(prompts[1], /Use the skill "two"/);
});

test("{input} is replaced with the user's context", async () => {
	const { prompts, deps } = recorder();
	await executeFlow(flow({ skill: "summarize", prompt: "Summarize {input} twice" }), "the Q3 report", deps);
	assert.match(prompts[0], /Summarize the Q3 report twice/);
});

test("runStep receives the step itself and its index, not just the prompt", async () => {
	// The loop stays agnostic about per-step settings — it hands the whole step
	// to the caller, so fields added later (e.g. a per-step model) need no
	// change here. Gates are not forwarded to runStep.
	const { seen, deps } = recorder();
	await executeFlow(
		flow(
			{ skill: "first", prompt: "x" },
			{ skill: APPROVAL_GATE, prompt: "?" },
			{ skill: "second", prompt: "y" },
		),
		"",
		{ ...deps, requestApproval: async () => true },
	);
	assert.deepEqual(seen, [
		{ skill: "first", index: 0 },
		{ skill: "second", index: 2 }, // index is the position in the flow, gates included
	]);
});

// ── Approval gates ─────────────────────────────────────────────────────

test("approved gate lets the flow continue", async () => {
	const { deps } = recorder();
	const r = await executeFlow(
		flow(
			{ skill: "one", prompt: "do one" },
			{ skill: APPROVAL_GATE, prompt: "ok to send?", channel: "telegram" },
			{ skill: "two", prompt: "do two" },
		),
		"",
		{ ...deps, requestApproval: async () => true },
	);

	assert.equal(r.completed, true);
	assert.deepEqual(r.steps.map((s) => s.skill), ["one", "two"]); // gate is not a step result
	assert.match(r.context, /approval gate\]: Approved via telegram/);
});

test("denied gate stops the flow and skips every later step", async () => {
	const { prompts, deps } = recorder();
	const r = await executeFlow(
		flow(
			{ skill: "one", prompt: "do one" },
			{ skill: APPROVAL_GATE, prompt: "ok to delete?" },
			{ skill: "destructive", prompt: "delete everything" },
		),
		"",
		{ ...deps, requestApproval: async () => false },
	);

	assert.equal(r.completed, false);
	assert.equal(r.abortedAt, 1);
	assert.match(r.abortReason!, /denied at step 2\/3/);
	assert.deepEqual(r.steps.map((s) => s.skill), ["one"]);
	assert.equal(prompts.length, 1); // "destructive" never ran
});

test("gate with no approval handler DENIES rather than silently continuing", async () => {
	const { prompts, deps } = recorder();
	const r = await executeFlow(
		flow(
			{ skill: APPROVAL_GATE, prompt: "ok to send the email?" },
			{ skill: "send-email", prompt: "send it" },
		),
		"",
		deps, // no requestApproval
	);

	assert.equal(r.completed, false);
	assert.equal(r.abortedAt, 0);
	assert.match(r.abortReason!, /no approval handler available/);
	assert.equal(prompts.length, 0);
});

test("custom gate prompt is used as the approval message", async () => {
	const seen: string[] = [];
	const { deps } = recorder();
	await executeFlow(flow({ skill: APPROVAL_GATE, prompt: "Ship to production?" }), "", {
		...deps,
		requestApproval: async (msg) => { seen.push(msg); return true; },
	});
	assert.match(seen[0], /Approval Required: Ship to production\?/);
});

test("gate with no prompt falls back to a summary of progress so far", async () => {
	const seen: string[] = [];
	const { deps } = recorder(["THE-RESULT"]);
	await executeFlow(
		flow({ skill: "one", prompt: "do one" }, { skill: APPROVAL_GATE, prompt: "" }),
		"",
		{ ...deps, requestApproval: async (msg) => { seen.push(msg); return true; } },
	);
	assert.match(seen[0], /paused at step 2\/2/);
	assert.match(seen[0], /THE-RESULT/);
});

// ── Progress events ────────────────────────────────────────────────────

test("emits a start/step/done event stream", async () => {
	const events: FlowEvent[] = [];
	const { deps } = recorder();
	await executeFlow(flow({ skill: "one", prompt: "x" }, { skill: "two", prompt: "y" }), "", {
		...deps,
		onProgress: (e) => events.push(e),
	});

	assert.deepEqual(events.map((e) => e.type), [
		"flow_start", "step_start", "step_done", "step_start", "step_done", "flow_done",
	]);
	assert.deepEqual(events[0], { type: "flow_start", flow: "test-flow", totalSteps: 2 });
});

test("emits approval and abort events when a gate denies", async () => {
	const events: FlowEvent[] = [];
	const { deps } = recorder();
	await executeFlow(flow({ skill: APPROVAL_GATE, prompt: "?" }), "", {
		...deps,
		requestApproval: async () => false,
		onProgress: (e) => events.push(e),
	});

	assert.deepEqual(events.map((e) => e.type), [
		"flow_start", "approval_requested", "approval_resolved", "flow_aborted",
	]);
});

// ── Error handling ─────────────────────────────────────────────────────

test("a failing step propagates to the caller", async () => {
	await assert.rejects(
		executeFlow(flow({ skill: "boom", prompt: "x" }), "", {
			runStep: async () => { throw new Error("model exploded"); },
		}),
		/model exploded/,
	);
});

test("an empty flow completes without running anything", async () => {
	const { prompts, deps } = recorder();
	const r = await executeFlow(flow(), "", deps);
	assert.equal(r.completed, true);
	assert.equal(r.steps.length, 0);
	assert.equal(prompts.length, 0);
});
