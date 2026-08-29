import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

let diffLines: typeof import("../dist/text-diff.js").diffLines;
let renderDiff: typeof import("../dist/text-diff.js").renderDiff;
let createConsoleElicitor: typeof import("../dist/elicit.js").createConsoleElicitor;
let createTaskTrackerTool: typeof import("../dist/tools/task-tracker.js").createTaskTrackerTool;
let createSkillLearnerTool: typeof import("../dist/tools/skill-learner.js").createSkillLearnerTool;

before(async () => {
	({ diffLines, renderDiff } = await import("../dist/text-diff.js"));
	({ createConsoleElicitor } = await import("../dist/elicit.js"));
	({ createTaskTrackerTool } = await import("../dist/tools/task-tracker.js"));
	({ createSkillLearnerTool } = await import("../dist/tools/skill-learner.js"));
});

const strip = (s: string) => s.replace(/\x1b\[[0-9;]*m/g, "");

describe("diffLines", () => {
	it("keeps common lines as context and marks the rest", () => {
		const out = diffLines("a\nb\nc", "a\nB\nc");
		assert.deepEqual(out, [
			{ sign: " ", text: "a" },
			{ sign: "-", text: "b" },
			{ sign: "+", text: "B" },
			{ sign: " ", text: "c" },
		]);
	});

	it("reports no changes for identical text", () => {
		const out = diffLines("1. one\n2. two", "1. one\n2. two");
		assert.ok(out.every((l) => l.sign === " "));
	});

	it("marks pure additions and pure deletions", () => {
		assert.deepEqual(
			diffLines("a", "a\nb").filter((l) => l.sign !== " "),
			[{ sign: "+", text: "b" }],
		);
		assert.deepEqual(
			diffLines("a\nb", "a").filter((l) => l.sign !== " "),
			[{ sign: "-", text: "b" }],
		);
	});
});

describe("renderDiff", () => {
	it("prefixes labelled headers and signs each line", () => {
		const text = strip(renderDiff("old", "new", { beforeLabel: "current", afterLabel: "proposed" }));
		assert.match(text, /--- current/);
		assert.match(text, /\+\+\+ proposed/);
		assert.match(text, /^ {2}- old$/m);
		assert.match(text, /^ {2}\+ new$/m);
	});
});

describe("ConsoleElicitor (non-interactive)", () => {
	it("is non-interactive without a TTY and returns the default key without reading stdin", async () => {
		const e = createConsoleElicitor();
		assert.equal(e.interactive, process.stdin.isTTY === true);
		if (e.interactive) return; // only assert the headless contract under `node --test`
		assert.equal(await e.select({ title: "t", choices: [{ key: "a", label: "accept" }], defaultKey: "a" }), "a");
		assert.equal(await e.edit("text"), null);
	});
});

describe("task_tracker flagged-skill gate", () => {
	const makeAgentDir = (confidence: number) => {
		const dir = mkdtempSync(join(tmpdir(), "gitagent-elicit-test-"));
		const skillDir = join(dir, "skills", "widget-checklist");
		mkdirSync(skillDir, { recursive: true });
		writeFileSync(
			join(skillDir, "SKILL.md"),
			`---\nname: widget-checklist\ndescription: Generate a widget checklist file\nconfidence: ${confidence}\nusage_count: 6\nsuccess_count: 2\nfailure_count: 4\nnegative_examples:\n  - wrote the file to the wrong directory\n---\n\n## Steps\n1. do the thing\n`,
		);
		return dir;
	};

	it("routes a user 'repair' decision into the tool result", async () => {
		const dir = makeAgentDir(0.3);
		const elicit = {
			interactive: true,
			select: async () => "r",
			edit: async () => null,
		};
		const tool = createTaskTrackerTool(dir, dir, undefined, undefined, elicit);
		const res = await tool.execute("c1", { action: "begin", objective: "Generate a widget checklist" });
		const text = res.content[0].text;
		assert.match(text, /USER DECISION: repair the skill before using it/);
		assert.match(text, /skill_learner action "repair" with skill_name "widget-checklist"/);
		assert.equal(res.details?.flagged_decision, "r");
	});

	it("routes a user 'skip' decision into the tool result", async () => {
		const dir = makeAgentDir(0.3);
		const elicit = { interactive: true, select: async () => "s", edit: async () => null };
		const tool = createTaskTrackerTool(dir, dir, undefined, undefined, elicit);
		const res = await tool.execute("c1", { action: "begin", objective: "Generate a widget checklist" });
		assert.match(res.content[0].text, /USER DECISION: skip the skill entirely/);
	});

	it("tells the model repair is disabled when nobody can approve it", async () => {
		const dir = makeAgentDir(0.3);
		const elicit = { interactive: false, select: async () => "r", edit: async () => null };
		const tool = createTaskTrackerTool(dir, dir, undefined, undefined, elicit, false);
		const res = await tool.execute("c1", { action: "begin", objective: "Generate a widget checklist" });
		const text = res.content[0].text;
		assert.doesNotMatch(text, /USER DECISION/);
		assert.match(text, /repair is DISABLED/);
		assert.match(text, /Do NOT call skill_learner action "repair"/);
		assert.equal(res.details?.flagged_decision, undefined);
	});

	it("points the model at repair when autoRepair is on and nobody can be asked", async () => {
		const dir = makeAgentDir(0.3);
		const tool = createTaskTrackerTool(dir, dir, undefined, undefined, undefined, true);
		const res = await tool.execute("c1", { action: "begin", objective: "Generate a widget checklist" });
		const text = res.content[0].text;
		assert.match(text, /automatic repair is ENABLED/);
		assert.match(text, /skill_learner action "repair" with skill_name "widget-checklist"/);
	});

	it("prefers the interactive prompt over autoRepair when a human is present", async () => {
		const dir = makeAgentDir(0.3);
		const elicit = { interactive: true, select: async () => "s", edit: async () => null };
		const tool = createTaskTrackerTool(dir, dir, undefined, undefined, elicit, true);
		const res = await tool.execute("c1", { action: "begin", objective: "Generate a widget checklist" });
		assert.match(res.content[0].text, /USER DECISION: skip the skill entirely/);
	});

	it("refuses skill_learner repair — without spending an LLM call — when nothing can approve it", async () => {
		const dir = makeAgentDir(0.3);
		const before = readFileSync(join(dir, "skills", "widget-checklist", "SKILL.md"), "utf-8");
		// A model object that would throw if the repair actually tried to use it:
		// the refusal must short-circuit before any completion is requested.
		const model = new Proxy({}, { get: () => { throw new Error("model must not be used"); } }) as any;
		const tool = createSkillLearnerTool(dir, dir, model, undefined, undefined, false);
		const res = await tool.execute("c1", { action: "repair", skill_name: "widget-checklist" });
		assert.match(res.content[0].text, /was NOT applied/);
		assert.equal(res.details?.approved, false);
		assert.equal(res.details?.reason, "no_approval_channel");
		assert.equal(readFileSync(join(dir, "skills", "widget-checklist", "SKILL.md"), "utf-8"), before);
	});

	it("does not gate a healthy skill match", async () => {
		const dir = makeAgentDir(0.9);
		let asked = false;
		const elicit = {
			interactive: true,
			select: async () => {
				asked = true;
				return "p";
			},
			edit: async () => null,
		};
		const tool = createTaskTrackerTool(dir, dir, undefined, undefined, elicit);
		const res = await tool.execute("c1", { action: "begin", objective: "Generate a widget checklist" });
		assert.equal(asked, false);
		assert.match(res.content[0].text, /YOU MUST USE IT/);
		// sanity: the fixture is actually being read back
		assert.ok(readFileSync(join(dir, "skills", "widget-checklist", "SKILL.md"), "utf-8").includes("confidence: 0.9"));
	});
});
