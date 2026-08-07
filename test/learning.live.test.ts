import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/**
 * Live-API coverage for the parts of the learning layer that need a real model:
 * the one-off completion helper, failure reflection, skill repair, and the full
 * repair write path behind the approval gate.
 *
 * Skipped unless you opt in — `npm test` stays hermetic:
 *
 *   GITAGENT_LIVE_TESTS=1 node --test test/learning.live.test.ts --experimental-strip-types
 *
 * Needs ANTHROPIC_API_KEY, or GITAGENT_MODEL=provider:model plus that key.
 */

const LIVE = process.env.GITAGENT_LIVE_TESTS === "1";
const MODEL_ID = process.env.GITAGENT_MODEL || "anthropic:claude-sonnet-4-6";

let runOneOffCompletion: typeof import("../dist/learning/llm-call.js").runOneOffCompletion;
let reflectOnFailure: typeof import("../dist/learning/reflection.js").reflectOnFailure;
let repairSkillSteps: typeof import("../dist/learning/skill-repair.js").repairSkillSteps;
let createSkillLearnerTool: typeof import("../dist/tools/skill-learner.js").createSkillLearnerTool;
let model: any;

before(async () => {
	if (!LIVE) return;
	({ runOneOffCompletion } = await import("../dist/learning/llm-call.js"));
	({ reflectOnFailure } = await import("../dist/learning/reflection.js"));
	({ repairSkillSteps } = await import("../dist/learning/skill-repair.js"));
	({ createSkillLearnerTool } = await import("../dist/tools/skill-learner.js"));
	const { loadAgent } = await import("../dist/loader.js");

	const dir = mkdtempSync(join(tmpdir(), "gitagent-live-agent-"));
	writeFileSync(join(dir, "agent.yaml"), `spec_version: "0.1.0"
name: live-test-agent
version: 0.1.0
description: Model holder for live learning tests
model:
  preferred: "${MODEL_ID}"
  fallback: []
tools: [read]
runtime:
  max_turns: 4
  timeout: 60
`);
	model = (await loadAgent(dir)).model;
});

// ── Fixtures ────────────────────────────────────────────────────────────

const FLAGGED_FM = `---
name: flaky-checklist
description: Generate a laptop setup checklist covering OS updates, software installation and backup configuration, saved in the workspace directory.
confidence: 0.3
usage_count: 7
success_count: 2
failure_count: 5
negative_examples:
  - Saved the checklist outside the workspace directory so the user could not find it
  - Assumed macOS and emitted steps that are invalid on Windows
---

## Steps
1. Identify the workspace directory.
2. Write a checklist file covering OS updates, software and backups.
3. Tell the user it is done.
`;

function flaggedAgentDir(): string {
	const dir = mkdtempSync(join(tmpdir(), "gitagent-live-"));
	mkdirSync(join(dir, "skills", "flaky-checklist"), { recursive: true });
	writeFileSync(join(dir, "skills", "flaky-checklist", "SKILL.md"), FLAGGED_FM);
	return dir;
}

const skillMd = (dir: string) => readFileSync(join(dir, "skills", "flaky-checklist", "SKILL.md"), "utf-8");
const stepsOf = (md: string) => md.match(/## Steps\r?\n([\s\S]*?)(?=\r?\n## |\s*$)/)?.[1].trim() ?? "";

const acceptElicitor = () => ({ interactive: true, select: async () => "a", edit: async () => null });

// ── llm-call.ts ─────────────────────────────────────────────────────────

describe("runOneOffCompletion (live)", { skip: !LIVE }, () => {
	it("returns trimmed text and reports usage to onUsage", async () => {
		const seen: any[] = [];
		const out = await runOneOffCompletion(
			model,
			"Answer with a single word, no punctuation.",
			"What is the capital of France?",
			{ maxTokens: 20 },
			(msg) => seen.push(msg),
		);
		assert.equal(out, out.trim());
		assert.match(out, /Paris/i);
		assert.equal(seen.length, 1);
		assert.equal(seen[0].type, "assistant");
		assert.ok(seen[0].usage.outputTokens > 0, "output tokens should be reported");
		assert.equal(typeof seen[0].usage.costUsd, "number");
	});

	it("throws instead of hanging when it times out", async () => {
		await assert.rejects(
			runOneOffCompletion(model, "You are terse.", "Write a 500 word essay about spline reticulation.", { timeoutMs: 1 }),
		);
	});
});

// ── reflection.ts ───────────────────────────────────────────────────────

describe("reflectOnFailure (live)", { skip: !LIVE }, () => {
	it("returns one bounded single-line paragraph grounded in the steps", async () => {
		const out = await reflectOnFailure(model, {
			objective: "Save a laptop setup checklist for the user",
			steps: [
				"Composed the checklist in memory",
				"Wrote the file to /tmp instead of the workspace directory",
				"Told the user the file was ready",
			],
			failureReason: "user could not find the file",
		});
		assert.ok(out.length > 0 && out.length <= 500, `length was ${out.length}`);
		assert.doesNotMatch(out, /\n/, "must be a single paragraph");
		assert.doesNotMatch(out, /^(Root cause|Here is)/i, "no preamble");
		assert.doesNotMatch(out, /^[-*#]/, "no markdown bullets or headers");
	});

	it("still reflects when no steps or reason were recorded", async () => {
		const out = await reflectOnFailure(model, { objective: "Do something vague", steps: [] });
		assert.ok(out.length > 0 && out.length <= 500);
	});
});

// ── skill-repair.ts ─────────────────────────────────────────────────────

describe("repairSkillSteps (live)", { skip: !LIVE }, () => {
	it("returns a bare numbered list with no fences, within the char cap", async () => {
		const out = await repairSkillSteps(model, {
			skillDescription: "Generate a laptop setup checklist saved in the workspace directory",
			currentSteps: "1. Identify the workspace directory.\n2. Write the checklist.\n3. Tell the user it is done.",
			negativeExamples: [
				"Saved the checklist outside the workspace directory",
				"Assumed macOS and emitted steps invalid on Windows",
			],
		});
		assert.match(out, /^1\./, "should start at step 1");
		assert.doesNotMatch(out, /```/, "no code fences");
		assert.doesNotMatch(out, /^#/m, "no markdown headers");
		assert.ok(out.length <= 3000, `length was ${out.length}`);
		assert.ok(out.split("\n").length >= 3, "should keep multiple steps");
	});
});

// ── skill_learner repair: the full write path ───────────────────────────

describe("skill_learner repair (live)", { skip: !LIVE }, () => {
	it("writes approved steps, resets stats, and records who approved", async () => {
		const dir = flaggedAgentDir();
		const before = skillMd(dir);
		const tool = createSkillLearnerTool(dir, dir, model, undefined, acceptElicitor(), false);
		const res = await tool.execute("c", { action: "repair", skill_name: "flaky-checklist" });

		assert.match(res.content[0].text, /repaired \(attempt 1\/3\) and committed/);
		assert.equal((res.details as any).approved, true);
		assert.equal((res.details as any).user_edited, false);

		const after = skillMd(dir);
		assert.notEqual(after, before);
		assert.match(after, /confidence: 0\.6/);
		assert.match(after, /repair_count: 1/);
		assert.match(after, /usage_count: 0/);
		assert.match(after, /negative_examples: \[\]/);
		assert.match(after, /## Repair History/);
		assert.match(after, /Repair #1 on .* \(user-approved\)/);
		// The recorded lessons are kept in the history even though the live array is cleared.
		assert.match(after, /Saved the checklist outside the workspace directory/);
		assert.ok(stepsOf(after).startsWith("1."));
	});

	it("keeps the user's hand-edited steps and labels the history accordingly", async () => {
		const dir = flaggedAgentDir();
		let edited = false;
		const elicit = {
			interactive: true,
			// accept only after one edit round, mirroring the real re-preview loop
			select: async () => (edited ? "a" : "e"),
			edit: async (initial: string) => { edited = true; return `1. HAND-EDITED FIRST STEP.\n${initial}`; },
		};
		const tool = createSkillLearnerTool(dir, dir, model, undefined, elicit, false);
		const res = await tool.execute("c", { action: "repair", skill_name: "flaky-checklist" });

		assert.equal((res.details as any).user_edited, true);
		assert.match(res.content[0].text, /The user edited and approved/);
		const after = skillMd(dir);
		assert.match(after, /HAND-EDITED FIRST STEP/);
		assert.match(after, /\(user-edited, approved\)/);
	});

	it("ignores an emptied editor buffer instead of writing a stepless skill", async () => {
		const dir = flaggedAgentDir();
		let tried = false;
		const elicit = {
			interactive: true,
			select: async () => (tried ? "a" : "e"),
			// Simulates the user clearing the whole file and saving.
			edit: async () => { tried = true; return "   \n\n"; },
		};
		const tool = createSkillLearnerTool(dir, dir, model, undefined, elicit, false);
		const res = await tool.execute("c", { action: "repair", skill_name: "flaky-checklist" });

		assert.match(res.content[0].text, /repaired \(attempt 1\/3\)/);
		assert.equal((res.details as any).user_edited, false, "an empty buffer is not an edit");
		const steps = stepsOf(skillMd(dir));
		assert.ok(steps.length > 0, "steps must not be empty");
		assert.ok(steps.startsWith("1."), `steps were: ${JSON.stringify(steps)}`);
	});

	it("leaves the file byte-identical when the user cancels", async () => {
		const dir = flaggedAgentDir();
		const before = skillMd(dir);
		const elicit = { interactive: true, select: async () => "c", edit: async () => null };
		const tool = createSkillLearnerTool(dir, dir, model, undefined, elicit, false);
		const res = await tool.execute("c", { action: "repair", skill_name: "flaky-checklist" });

		assert.match(res.content[0].text, /CANCELLED BY THE USER/);
		assert.equal((res.details as any).cancelled, true);
		assert.equal(skillMd(dir), before);
	});

	it("applies unattended under autoRepair and labels the history", async () => {
		const dir = flaggedAgentDir();
		const tool = createSkillLearnerTool(dir, dir, model, undefined, undefined, true);
		const res = await tool.execute("c", { action: "repair", skill_name: "flaky-checklist" });

		assert.match(res.content[0].text, /repaired \(attempt 1\/3\)/);
		assert.doesNotMatch(res.content[0].text, /The user/);
		assert.match(skillMd(dir), /\(autoRepair\)/);
		assert.match(skillMd(dir), /confidence: 0\.6/);
	});

	it("counts repairs across runs and stops at the third", async () => {
		const dir = flaggedAgentDir();
		const tool = createSkillLearnerTool(dir, dir, model, undefined, undefined, true);
		for (let i = 1; i <= 3; i++) {
			const res = await tool.execute("c", { action: "repair", skill_name: "flaky-checklist" });
			assert.match(res.content[0].text, new RegExp(`attempt ${i}/3`));
			// A repair resets confidence to 0.6, so re-flag it to allow the next one.
			writeFileSync(
				join(dir, "skills", "flaky-checklist", "SKILL.md"),
				skillMd(dir).replace(/confidence: 0\.6/, "confidence: 0.3"),
			);
		}
		await assert.rejects(
			tool.execute("c", { action: "repair", skill_name: "flaky-checklist" }),
			/already been repaired 3\/3 times/,
		);
	});
});
