import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Everything on this branch that runs without a model: the reinforcement math,
// task_tracker's full lifecycle, and skill_learner's actions and guard rails.
// The LLM-dependent parts (reflection text, repair rewriting) are covered by
// learning.live.test.ts, which needs an API key.

let reinforcement: typeof import("../dist/learning/reinforcement.js");
let createTaskTrackerTool: typeof import("../dist/tools/task-tracker.js").createTaskTrackerTool;
let createSkillLearnerTool: typeof import("../dist/tools/skill-learner.js").createSkillLearnerTool;

before(async () => {
	reinforcement = await import("../dist/learning/reinforcement.js");
	({ createTaskTrackerTool } = await import("../dist/tools/task-tracker.js"));
	({ createSkillLearnerTool } = await import("../dist/tools/skill-learner.js"));
});

// ── Fixtures ────────────────────────────────────────────────────────────

const agentDir = () => mkdtempSync(join(tmpdir(), "gitagent-learning-"));

function writeSkill(dir: string, name: string, frontmatter: Record<string, unknown>, steps = "1. do the thing") {
	const skillDir = join(dir, "skills", name);
	mkdirSync(skillDir, { recursive: true });
	const fm = Object.entries(frontmatter)
		.map(([k, v]) => (Array.isArray(v)
			? `${k}:\n${v.map((x) => `  - ${x}`).join("\n")}`
			: `${k}: ${typeof v === "string" ? JSON.stringify(v) : v}`))
		.join("\n");
	writeFileSync(join(skillDir, "SKILL.md"), `---\n${fm}\n---\n\n## Steps\n${steps}\n`);
	return skillDir;
}

const skillFile = (dir: string, name: string) => join(dir, "skills", name, "SKILL.md");
const text = (res: any) => res.content[0].text as string;

/** A model that explodes on any use — proves a code path never reaches the LLM. */
const explodingModel = () =>
	new Proxy({}, { get: () => { throw new Error("model must not be used"); } }) as any;

const tracker = (dir: string, model?: any) => createTaskTrackerTool(dir, dir, model);
const learner = (dir: string, model?: any, elicit?: any, autoRepair?: boolean) =>
	createSkillLearnerTool(dir, dir, model, undefined, elicit, autoRepair);

/** begin → N steps → end(outcome). Returns the task id. */
async function runTask(dir: string, objective: string, steps: string[], outcome: string, extra: Record<string, unknown> = {}) {
	const t = tracker(dir);
	const begun = await t.execute("c", { action: "begin", objective });
	const taskId = (begun.details as any).task_id as string;
	for (const step of steps) await t.execute("c", { action: "update", task_id: taskId, step });
	await t.execute("c", { action: "end", task_id: taskId, outcome, ...extra });
	return taskId;
}

// ── reinforcement.ts ────────────────────────────────────────────────────

describe("adjustConfidence", () => {
	const base = { confidence: 0.5, usage_count: 3, success_count: 2, failure_count: 1, negative_examples: [] as string[] };

	it("moves success asymptotically toward 1.0", () => {
		const out = reinforcement.adjustConfidence(base, "success");
		assert.equal(out.confidence, 0.55); // 0.5 + 0.1 * (1 - 0.5)
		assert.equal(out.success_count, 3);
		assert.equal(out.usage_count, 4);
		assert.equal(out.failure_count, 1);
	});

	it("never exceeds 1.0 on success", () => {
		assert.equal(reinforcement.adjustConfidence({ ...base, confidence: 1 }, "success").confidence, 1);
	});

	it("penalises failure twice as hard as success rewards", () => {
		const out = reinforcement.adjustConfidence(base, "failure", "wrote to the wrong path");
		assert.equal(out.confidence, 0.3); // 0.5 - 0.2
		assert.equal(out.failure_count, 2);
		assert.deepEqual(out.negative_examples, ["wrote to the wrong path"]);
	});

	it("treats partial as a small penalty that still counts as a failure", () => {
		const out = reinforcement.adjustConfidence(base, "partial", "half done");
		assert.equal(out.confidence, 0.45);
		assert.equal(out.failure_count, 2);
		assert.deepEqual(out.negative_examples, ["half done"]);
	});

	it("floors confidence at 0.0", () => {
		assert.equal(reinforcement.adjustConfidence({ ...base, confidence: 0.1 }, "failure").confidence, 0);
	});

	it("records no lesson when no reason is given", () => {
		assert.deepEqual(reinforcement.adjustConfidence(base, "failure").negative_examples, []);
	});

	it("caps negative_examples at 10, dropping the oldest", () => {
		let stats = { ...base, negative_examples: [] as string[] };
		for (let i = 1; i <= 12; i++) stats = reinforcement.adjustConfidence(stats, "failure", `lesson ${i}`);
		assert.equal(stats.negative_examples.length, 10);
		assert.equal(stats.negative_examples[0], "lesson 3");
		assert.equal(stats.negative_examples.at(-1), "lesson 12");
	});

	it("keeps confidence at two decimals (no float drift)", () => {
		let stats = { ...base, confidence: 0.35 };
		for (let i = 0; i < 6; i++) stats = reinforcement.adjustConfidence(stats, "success");
		assert.equal(String(stats.confidence), String(Math.round(stats.confidence * 100) / 100));
	});

	it("does not mutate the input stats", () => {
		const input = { ...base, negative_examples: ["old"] };
		reinforcement.adjustConfidence(input, "failure", "new");
		assert.equal(input.usage_count, 3);
		assert.deepEqual(input.negative_examples, ["old"]);
	});
});

describe("isSkillFlagged", () => {
	it("flags below 0.4 only", () => {
		const s = (confidence: number) => ({ confidence, usage_count: 0, success_count: 0, failure_count: 0, negative_examples: [] });
		assert.equal(reinforcement.isSkillFlagged(s(0.39)), true);
		assert.equal(reinforcement.isSkillFlagged(s(0.4)), false);
		assert.equal(reinforcement.isSkillFlagged(s(0)), true);
	});
});

describe("loadSkillStats / saveSkillStats", () => {
	it("defaults to full confidence when there is no SKILL.md", async () => {
		const stats = await reinforcement.loadSkillStats(join(agentDir(), "nope"));
		assert.deepEqual(stats, { confidence: 1, usage_count: 0, success_count: 0, failure_count: 0, negative_examples: [] });
	});

	it("fills in defaults for missing or malformed fields", async () => {
		const dir = agentDir();
		const skillDir = writeSkill(dir, "partial", { name: "partial", confidence: 0.5, usage_count: "not-a-number" });
		const stats = await reinforcement.loadSkillStats(skillDir);
		assert.equal(stats.confidence, 0.5);
		assert.equal(stats.usage_count, 0);
		assert.deepEqual(stats.negative_examples, []);
	});

	it("round-trips stats while preserving the skill body", async () => {
		const dir = agentDir();
		const skillDir = writeSkill(dir, "rt", { name: "rt", confidence: 1 }, "1. keep me");
		await reinforcement.saveSkillStats(skillDir, {
			confidence: 0.2, usage_count: 5, success_count: 1, failure_count: 4, negative_examples: ["a", "b"],
		});
		const raw = readFileSync(join(skillDir, "SKILL.md"), "utf-8");
		assert.match(raw, /confidence: 0\.2/);
		assert.match(raw, /1\. keep me/);
		assert.deepEqual((await reinforcement.loadSkillStats(skillDir)).negative_examples, ["a", "b"]);
	});
});

// ── task_tracker ────────────────────────────────────────────────────────

describe("task_tracker begin", () => {
	it("starts a task and reports no skill match", async () => {
		const dir = agentDir();
		const res = await tracker(dir).execute("c", { action: "begin", objective: "Reticulate the splines" });
		assert.match(text(res), /Task started: /);
		assert.match(text(res), /No matching skills found\. Solve from scratch\./);
		assert.ok((res.details as any).task_id);
	});

	it("requires an objective", async () => {
		await assert.rejects(tracker(agentDir()).execute("c", { action: "begin" }), /objective is required/);
	});

	it("resumes an active task with the same objective and bumps the attempt", async () => {
		const dir = agentDir();
		const t = tracker(dir);
		const first = await t.execute("c", { action: "begin", objective: "Same job" });
		const again = await t.execute("c", { action: "begin", objective: "Same job" });
		assert.match(text(again), /Resuming task/);
		assert.match(text(again), /attempt #2/);
		assert.equal((again.details as any).task_id, (first.details as any).task_id);
	});

	it("replays prior failure reasons on a fresh attempt", async () => {
		const dir = agentDir();
		await runTask(dir, "Flaky job", ["step one"], "failure", { failure_reason: "picked the wrong directory" });
		const res = await tracker(dir).execute("c", { action: "begin", objective: "Flaky job" });
		assert.match(text(res), /Attempt #2/);
		assert.match(text(res), /Prior failures:/);
		assert.match(text(res), /picked the wrong directory/);
		assert.match(text(res), /Avoid these approaches/);
	});

	it("orders a healthy skill match to be used", async () => {
		const dir = agentDir();
		writeSkill(dir, "widget-checklist", { name: "widget-checklist", description: "Generate a widget checklist file", confidence: 0.9 });
		const res = await tracker(dir).execute("c", { action: "begin", objective: "Generate a widget checklist" });
		assert.match(text(res), /YOU MUST USE IT/);
		assert.match(text(res), /Load skills\/widget-checklist\/SKILL\.md NOW/);
	});

	it("lists runner-up matches under the top one", async () => {
		const dir = agentDir();
		writeSkill(dir, "widget-checklist", { name: "widget-checklist", description: "Generate a widget checklist file", confidence: 0.9 });
		writeSkill(dir, "widget-report", { name: "widget-report", description: "Generate a widget report file summary", confidence: 0.8 });
		const res = await tracker(dir).execute("c", { action: "begin", objective: "Generate a widget checklist file" });
		assert.match(text(res), /Other matching skills:/);
		assert.match(text(res), /widget-report/);
	});
});

describe("task_tracker update", () => {
	it("records numbered steps", async () => {
		const dir = agentDir();
		const t = tracker(dir);
		const begun = await t.execute("c", { action: "begin", objective: "Multi step job" });
		const id = (begun.details as any).task_id;
		assert.match(text(await t.execute("c", { action: "update", task_id: id, step: "first" })), /Step 1 recorded/);
		assert.match(text(await t.execute("c", { action: "update", task_id: id, step: "second" })), /Step 2 recorded/);
	});

	it("rejects unknown, inactive, and stepless updates", async () => {
		const dir = agentDir();
		const t = tracker(dir);
		await assert.rejects(t.execute("c", { action: "update", task_id: "nope", step: "x" }), /Task not found/);
		const begun = await t.execute("c", { action: "begin", objective: "Short job" });
		const id = (begun.details as any).task_id;
		await assert.rejects(t.execute("c", { action: "update", task_id: id }), /step is required/);
		await t.execute("c", { action: "end", task_id: id, outcome: "success" });
		await assert.rejects(t.execute("c", { action: "update", task_id: id, step: "late" }), /is not active/);
	});
});

describe("task_tracker end", () => {
	it("rewards the skill it used on success", async () => {
		const dir = agentDir();
		writeSkill(dir, "widget-checklist", { name: "widget-checklist", description: "widget checklist", confidence: 0.5, usage_count: 1, success_count: 1, failure_count: 0 });
		const t = tracker(dir);
		const begun = await t.execute("c", { action: "begin", objective: "Unrelated objective wording" });
		const id = (begun.details as any).task_id;
		const res = await t.execute("c", { action: "end", task_id: id, outcome: "success", skill_used: "widget-checklist" });
		assert.match(text(res), /completed successfully/);
		assert.match(text(res), /confidence: 0\.5 → 0\.55/);
		assert.match(readFileSync(skillFile(dir, "widget-checklist"), "utf-8"), /confidence: 0\.55/);
		assert.match(text(res), /skill_learner action "evaluate"/);
	});

	it("penalises the skill on failure and stores the lesson", async () => {
		const dir = agentDir();
		writeSkill(dir, "widget-checklist", { name: "widget-checklist", description: "widget checklist", confidence: 0.5 });
		const t = tracker(dir);
		const begun = await t.execute("c", { action: "begin", objective: "Unrelated objective wording" });
		const id = (begun.details as any).task_id;
		const res = await t.execute("c", {
			action: "end", task_id: id, outcome: "failure",
			failure_reason: "saved the file outside the workspace", skill_used: "widget-checklist",
		});
		assert.match(text(res), /confidence: 0\.5 → 0\.3/);
		assert.match(text(res), /saved the file outside the workspace/);
		assert.match(readFileSync(skillFile(dir, "widget-checklist"), "utf-8"), /saved the file outside the workspace/);
	});

	it("keeps the raw failure reason when reflection is unavailable (no model)", async () => {
		const dir = agentDir();
		const t = tracker(dir);
		const begun = await t.execute("c", { action: "begin", objective: "No model job" });
		const id = (begun.details as any).task_id;
		const res = await t.execute("c", { action: "end", task_id: id, outcome: "failure", failure_reason: "raw reason" });
		assert.match(text(res), /Reason: raw reason/);
	});

	it("fails soft when reflection itself blows up, keeping the raw reason", async () => {
		const dir = agentDir();
		// Tracks that reflection was really attempted — otherwise this test would
		// pass just as happily if the reflection call were never made at all.
		let touched = false;
		const model = new Proxy({}, {
			get: () => { touched = true; throw new Error("model exploded"); },
		}) as any;
		const t = tracker(dir, model);
		const begun = await t.execute("c", { action: "begin", objective: "Broken reflection job" });
		const id = (begun.details as any).task_id;
		const res = await t.execute("c", { action: "end", task_id: id, outcome: "failure", failure_reason: "raw reason survives" });
		assert.equal(touched, true, "reflection should have been attempted");
		assert.match(text(res), /Reason: raw reason survives/);
		assert.match(text(res), /Consider a different approach/);
	});

	it("reports a missing skill without failing the end call", async () => {
		const dir = agentDir();
		const t = tracker(dir);
		const begun = await t.execute("c", { action: "begin", objective: "Ghost skill job" });
		const id = (begun.details as any).task_id;
		const res = await t.execute("c", { action: "end", task_id: id, outcome: "success", skill_used: "does-not-exist" });
		assert.match(text(res), /Could not update skill "does-not-exist" stats/);
	});

	it("validates its arguments", async () => {
		const dir = agentDir();
		const t = tracker(dir);
		await assert.rejects(t.execute("c", { action: "end", outcome: "success" }), /task_id is required/);
		const begun = await t.execute("c", { action: "begin", objective: "Arg check job" });
		const id = (begun.details as any).task_id;
		await assert.rejects(t.execute("c", { action: "end", task_id: id }), /outcome is required/);
		await assert.rejects(t.execute("c", { action: "end", task_id: "nope", outcome: "success" }), /Task not found/);
	});
});

describe("task_tracker list", () => {
	it("shows active tasks only", async () => {
		const dir = agentDir();
		const t = tracker(dir);
		assert.match(text(await t.execute("c", { action: "list" })), /No active tasks/);
		await t.execute("c", { action: "begin", objective: "Still running" });
		await runTask(dir, "Already done", ["a"], "success");
		const res = await t.execute("c", { action: "list" });
		assert.match(text(res), /Still running/);
		assert.doesNotMatch(text(res), /Already done/);
		assert.equal((res.details as any).count, 1);
	});
});

// ── skill_learner ───────────────────────────────────────────────────────

describe("skill_learner evaluate", () => {
	it("accepts a multi-step novel success", async () => {
		const dir = agentDir();
		const id = await runTask(dir, "Assemble the quarterly widget report", ["one", "two", "three"], "success");
		const res = await learner(dir).execute("c", { action: "evaluate", task_id: id });
		assert.match(text(res), /Task IS worthy/);
		assert.equal((res.details as any).worthy, true);
	});

	it("rejects a task that did not succeed", async () => {
		const dir = agentDir();
		const id = await runTask(dir, "Failed job", ["one", "two", "three"], "failure", { failure_reason: "nope" });
		const res = await learner(dir).execute("c", { action: "evaluate", task_id: id });
		assert.match(text(res), /did not succeed/);
	});

	it("marks a task non-novel when an existing skill already covers it", async () => {
		const dir = agentDir();
		writeSkill(dir, "quarterly-widget-report", { name: "quarterly-widget-report", description: "Assemble the quarterly widget report", confidence: 1 });
		const id = await runTask(dir, "Assemble the quarterly widget report", ["one", "two", "three"], "success");
		const res = await learner(dir).execute("c", { action: "evaluate", task_id: id });
		assert.equal((res.details as any).checks.novel, false);
	});

	it("honours override_heuristic for a thin task", async () => {
		const dir = agentDir();
		const id = await runTask(dir, "Tiny job", ["only step"], "success");
		const plain = await learner(dir).execute("c", { action: "evaluate", task_id: id });
		assert.match(text(plain), /NOT worthy/);
		const forced = await learner(dir).execute("c", { action: "evaluate", task_id: id, override_heuristic: true });
		assert.match(text(forced), /Task IS worthy/);
	});

	it("validates its arguments", async () => {
		const dir = agentDir();
		await assert.rejects(learner(dir).execute("c", { action: "evaluate" }), /task_id is required/);
		await assert.rejects(learner(dir).execute("c", { action: "evaluate", task_id: "nope" }), /Task not found/);
	});
});

describe("skill_learner crystallize", () => {
	it("writes a SKILL.md with full confidence and the task's steps", async () => {
		const dir = agentDir();
		const id = await runTask(dir, "Assemble the widget report", ["gather data", "render report", "verify output"], "success");
		const res = await learner(dir).execute("c", {
			action: "crystallize", task_id: id, skill_name: "widget-report", skill_description: "Assemble a widget report",
		});
		assert.match(text(res), /crystallized and committed/);
		const md = readFileSync(skillFile(dir, "widget-report"), "utf-8");
		assert.match(md, /confidence: 1/);
		assert.match(md, /usage_count: 0/);
		assert.match(md, /learned_from: task:/);
		assert.match(md, /1\. gather data/);
		assert.match(md, /3\. verify output/);
		assert.match(md, /## What Worked/);
	});

	it("carries prior failures into a What Did NOT Work section", async () => {
		const dir = agentDir();
		await runTask(dir, "Repeat job", ["a"], "failure", { failure_reason: "used the wrong parser" });
		const id = await runTask(dir, "Repeat job", ["a", "b", "c"], "success");
		await learner(dir).execute("c", {
			action: "crystallize", task_id: id, skill_name: "repeat-job", skill_description: "Do the repeat job",
		});
		const md = readFileSync(skillFile(dir, "repeat-job"), "utf-8");
		assert.match(md, /## What Did NOT Work/);
		assert.match(md, /used the wrong parser/);
	});

	it("refuses to overwrite an existing skill, preserving its record", async () => {
		// Regression: a live voice session crystallized over a flagged skill,
		// resetting confidence 0.3 → 1.0, wiping its recorded failures, and
		// replacing its steps with the task's step log — all ungated.
		const dir = agentDir();
		writeSkill(dir, "widget-report", {
			name: "widget-report", description: "Assemble a widget report",
			confidence: 0.3, usage_count: 7, success_count: 2, failure_count: 5,
			negative_examples: ["used the wrong parser"],
		}, "1. original instructions");
		const before = readFileSync(skillFile(dir, "widget-report"), "utf-8");

		const id = await runTask(dir, "Assemble the widget report", ["gather", "render", "verify"], "success");
		const res = await learner(dir).execute("c", {
			action: "crystallize", task_id: id, skill_name: "widget-report", skill_description: "Assemble a widget report",
		});

		assert.match(text(res), /already exists/);
		assert.match(text(res), /does NOT overwrite/);
		assert.match(text(res), /Use action "update"/);
		assert.equal((res.details as any).created, false);
		assert.equal((res.details as any).reason, "already_exists");
		assert.equal(readFileSync(skillFile(dir, "widget-report"), "utf-8"), before);
	});

	it("refuses a failed task", async () => {
		const dir = agentDir();
		const id = await runTask(dir, "Doomed job", ["a", "b", "c"], "failure", { failure_reason: "nope" });
		await assert.rejects(
			learner(dir).execute("c", { action: "crystallize", task_id: id, skill_name: "doomed", skill_description: "d" }),
			/Cannot crystallize failed task/,
		);
		assert.equal(existsSync(skillFile(dir, "doomed")), false);
	});

	it("requires a kebab-case name and the other fields", async () => {
		const dir = agentDir();
		const id = await runTask(dir, "Naming job", ["a", "b", "c"], "success");
		await assert.rejects(
			learner(dir).execute("c", { action: "crystallize", task_id: id, skill_name: "Not Kebab", skill_description: "d" }),
			/must be kebab-case/,
		);
		await assert.rejects(learner(dir).execute("c", { action: "crystallize", task_id: id }), /skill_name is required/);
		await assert.rejects(
			learner(dir).execute("c", { action: "crystallize", task_id: id, skill_name: "fine-name" }),
			/skill_description is required/,
		);
	});
});

describe("skill_learner status / review", () => {
	it("marks flagged skills and counts repairs", async () => {
		const dir = agentDir();
		writeSkill(dir, "good-skill", { name: "good-skill", description: "d", confidence: 0.8, usage_count: 4, success_count: 4, failure_count: 0 });
		writeSkill(dir, "bad-skill", { name: "bad-skill", description: "d", confidence: 0.2, usage_count: 6, success_count: 2, failure_count: 4, repair_count: 1 });
		const res = await learner(dir).execute("c", { action: "status" });
		const out = text(res);
		assert.match(out, /good-skill: confidence=0\.8, usage=4, success_ratio=4\/4$/m);
		assert.match(out, /bad-skill: .*repairs=1\/3 ⚠️ FLAGGED/);
		assert.match(out, /1 skill\(s\) flagged as unreliable/);
	});

	it("reports an empty skills directory", async () => {
		assert.match(text(await learner(agentDir()).execute("c", { action: "status" })), /No skills directory found/);
	});

	it("review lists only flagged skills, with their lessons", async () => {
		const dir = agentDir();
		writeSkill(dir, "good-skill", { name: "good-skill", description: "d", confidence: 0.8 });
		writeSkill(dir, "bad-skill", { name: "bad-skill", description: "d", confidence: 0.2, negative_examples: ["wrong path", "wrong OS"] });
		const out = text(await learner(dir).execute("c", { action: "review" }));
		assert.match(out, /bad-skill: confidence=0\.2/);
		assert.doesNotMatch(out, /good-skill/);
		assert.match(out, /wrong path; wrong OS/);
	});

	it("review says so when nothing is flagged", async () => {
		const dir = agentDir();
		writeSkill(dir, "good-skill", { name: "good-skill", description: "d", confidence: 0.8 });
		assert.match(text(await learner(dir).execute("c", { action: "review" })), /No flagged skills/);
	});
});

describe("skill_learner repair guard rails", () => {
	// Every case here must bail out before the model is touched.
	it("requires a skill_name and a model", async () => {
		const dir = agentDir();
		await assert.rejects(learner(dir).execute("c", { action: "repair" }), /skill_name is required/);
		await assert.rejects(
			learner(dir, undefined).execute("c", { action: "repair", skill_name: "whatever" }),
			/Repair requires a model/,
		);
	});

	it("rejects an unknown skill and a malformed SKILL.md", async () => {
		const dir = agentDir();
		await assert.rejects(
			learner(dir, explodingModel()).execute("c", { action: "repair", skill_name: "ghost" }),
			/Skill not found: ghost/,
		);
		const badDir = join(dir, "skills", "no-frontmatter");
		mkdirSync(badDir, { recursive: true });
		writeFileSync(join(badDir, "SKILL.md"), "just a body, no frontmatter\n");
		await assert.rejects(
			learner(dir, explodingModel()).execute("c", { action: "repair", skill_name: "no-frontmatter" }),
			/Invalid SKILL\.md format/,
		);
	});

	it("refuses to repair a skill that is not flagged", async () => {
		const dir = agentDir();
		writeSkill(dir, "healthy", { name: "healthy", description: "d", confidence: 0.8 });
		await assert.rejects(
			learner(dir, explodingModel()).execute("c", { action: "repair", skill_name: "healthy" }),
			/is not flagged \(confidence 0\.8 >= 0\.4\)/,
		);
	});

	it("stops after MAX_REPAIRS and points at update/delete", async () => {
		const dir = agentDir();
		writeSkill(dir, "exhausted", { name: "exhausted", description: "d", confidence: 0.2, repair_count: 3 });
		await assert.rejects(
			learner(dir, explodingModel()).execute("c", { action: "repair", skill_name: "exhausted" }),
			/already been repaired 3\/3 times\. Use "update" or "delete"/,
		);
	});

	it("refuses when nothing can approve the repair, leaving the file untouched", async () => {
		const dir = agentDir();
		writeSkill(dir, "flagged", { name: "flagged", description: "d", confidence: 0.2 });
		const before = readFileSync(skillFile(dir, "flagged"), "utf-8");
		const res = await learner(dir, explodingModel(), undefined, false).execute("c", { action: "repair", skill_name: "flagged" });
		assert.match(text(res), /was NOT applied/);
		assert.equal((res.details as any).reason, "no_approval_channel");
		assert.equal(readFileSync(skillFile(dir, "flagged"), "utf-8"), before);
	});

	it("refuses the same way when a non-interactive elicitor is supplied", async () => {
		const dir = agentDir();
		writeSkill(dir, "flagged", { name: "flagged", description: "d", confidence: 0.2 });
		const elicit = { interactive: false, select: async () => "a", edit: async () => null };
		const res = await learner(dir, explodingModel(), elicit, false).execute("c", { action: "repair", skill_name: "flagged" });
		assert.match(text(res), /was NOT applied/);
	});
});

describe("skill_learner update / delete", () => {
	it("replaces the body and keeps the frontmatter", async () => {
		const dir = agentDir();
		writeSkill(dir, "editable", { name: "editable", description: "keep me", confidence: 0.5 }, "1. old step");
		const res = await learner(dir).execute("c", { action: "update", skill_name: "editable", instructions: "## Steps\n1. brand new step" });
		assert.match(text(res), /updated and committed/);
		const md = readFileSync(skillFile(dir, "editable"), "utf-8");
		assert.match(md, /description: keep me/);
		assert.match(md, /confidence: 0\.5/);
		assert.match(md, /1\. brand new step/);
		assert.doesNotMatch(md, /old step/);
	});

	it("validates update arguments", async () => {
		const dir = agentDir();
		await assert.rejects(learner(dir).execute("c", { action: "update", skill_name: "x" }), /instructions is required/);
		await assert.rejects(
			learner(dir).execute("c", { action: "update", skill_name: "ghost", instructions: "x" }),
			/Skill not found: ghost/,
		);
	});

	it("deletes a skill directory and rejects an unknown one", async () => {
		const dir = agentDir();
		writeSkill(dir, "doomed", { name: "doomed", description: "d", confidence: 0.5 });
		assert.match(text(await learner(dir).execute("c", { action: "delete", skill_name: "doomed" })), /deleted/);
		assert.equal(existsSync(join(dir, "skills", "doomed")), false);
		await assert.rejects(learner(dir).execute("c", { action: "delete", skill_name: "doomed" }), /Skill not found/);
	});
});

describe("both tools", () => {
	it("reject unknown actions", async () => {
		const dir = agentDir();
		await assert.rejects(tracker(dir).execute("c", { action: "nonsense" }), /Unknown action: nonsense/);
		await assert.rejects(learner(dir).execute("c", { action: "nonsense" }), /Unknown action: nonsense/);
	});

	it("honour an already-aborted signal", async () => {
		const dir = agentDir();
		const aborted = AbortSignal.abort();
		await assert.rejects(tracker(dir).execute("c", { action: "list" }, aborted), /Operation aborted/);
		await assert.rejects(learner(dir).execute("c", { action: "status" }, aborted), /Operation aborted/);
	});
});
