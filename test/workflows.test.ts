import assert from "node:assert/strict";
import { test } from "node:test";
import { executeFlow } from "../dist/workflows.js";

const flow = {
	name: "daily-report",
	description: "Build a report",
	steps: [
		{ skill: "collect", prompt: "Collect data" },
		{ skill: "__approval_gate__", prompt: "Approve publication", channel: "telegram" },
		{ skill: "publish", prompt: "Publish report" },
	],
};

test("executeFlow chains outputs and emits progress", async () => {
	const calls: string[] = [];
	const events: string[] = [];
	const result = await executeFlow(flow, "Monday", {
		runStep: async (step, input, index) => {
			calls.push(`${index}:${step.skill}:${input}`);
			return `${input}->${step.skill}`;
		},
		requestApproval: async (step, index) => {
			calls.push(`${index}:${step.skill}`);
			return true;
		},
		onProgress: (event) => events.push(event.type),
	});

	assert.equal(result.status, "completed");
	assert.equal(result.output, "Monday->collect->publish");
	assert.deepEqual(calls, ["0:collect:Monday", "1:__approval_gate__", "2:publish:Monday->collect"]);
	assert.deepEqual(events, ["start", "step_start", "step_complete", "step_start", "step_start", "step_complete", "complete"]);
});

test("executeFlow stops safely when an approval gate is rejected", async () => {
	let runCount = 0;
	const result = await executeFlow(flow, "input", {
		runStep: async (_step, input) => {
			runCount++;
			return `${input}:collected`;
		},
		requestApproval: async () => false,
	});

	assert.equal(result.status, "rejected");
	assert.equal(result.output, "input:collected");
	assert.equal(result.steps.length, 1);
	assert.equal(runCount, 1);
});

test("executeFlow requires an approval callback for approval gates", async () => {
	await assert.rejects(
		executeFlow(flow, "input", { runStep: async (_step, input) => input }),
		/requires an approval callback/,
	);
});
