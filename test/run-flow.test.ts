// Tests for the runFlow convenience wrapper. These deliberately use flows whose
// only step is an approval gate — a gate is resolved before any agent call, so
// the whole wrapper (flow lookup, approval policy, result shape) is exercised
// without needing an API key or spending a token.

import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Imported from dist/ (not src/) because run-flow.ts has runtime imports of
// sibling modules using ".js" specifiers, which don't resolve under
// --experimental-strip-types. Same convention as test/mcp.test.ts.
import { runFlow } from "../dist/run-flow.js";

async function agentWith(files: Record<string, string>): Promise<string> {
	const dir = await mkdtemp(join(tmpdir(), "gitagent-runflow-"));
	await mkdir(join(dir, "workflows"), { recursive: true });
	for (const [name, body] of Object.entries(files)) {
		await writeFile(join(dir, "workflows", name), body, "utf-8");
	}
	return dir;
}

const GATE_ONLY = `name: gate-only
description: A single approval gate, nothing else
steps:
  - skill: __approval_gate__
    prompt: "Continue?"
    channel: telegram
`;

// ── Flow lookup ────────────────────────────────────────────────────────

test("unknown flow name errors and lists what is available", async () => {
	const dir = await agentWith({ "gate-only.yaml": GATE_ONLY });
	try {
		await assert.rejects(
			runFlow({ agentDir: dir, flow: "does-not-exist" }),
			/Flow "does-not-exist" not found.*available: gate-only/s,
		);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

test("a runnable flow wins over a same-named reference workflow", async () => {
	// Both discover as "gate-only". readdir order is filesystem-dependent, so
	// resolution must prefer the runnable one rather than whichever came first.
	const dir = await agentWith({
		"gate-only.md": "---\nname: gate-only\ndescription: a doc that shadows the flow\n---\n\nProse.\n",
		"gate-only.yaml": GATE_ONLY,
	});
	try {
		const r = await runFlow({ agentDir: dir, flow: "gate-only", approve: "auto" });
		assert.equal(r.completed, true); // resolved the YAML, not the markdown
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

test("a markdown reference workflow is rejected as not runnable", async () => {
	const dir = await agentWith({
		"notes.md": "---\nname: notes\ndescription: just a reference doc\n---\n\nSome prose.\n",
	});
	try {
		await assert.rejects(
			runFlow({ agentDir: dir, flow: "notes" }),
			/reference workflow.*not a runnable SkillFlow/s,
		);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

// ── Approval policy ────────────────────────────────────────────────────

test("gates deny by default — no policy given", async () => {
	const dir = await agentWith({ "gate-only.yaml": GATE_ONLY });
	try {
		const r = await runFlow({ agentDir: dir, flow: "gate-only" });
		assert.equal(r.completed, false);
		assert.match(r.abortReason!, /no approval handler available/);
		assert.deepEqual(r.usage, []);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

test('approve: "deny" is explicit but behaves the same', async () => {
	const dir = await agentWith({ "gate-only.yaml": GATE_ONLY });
	try {
		const r = await runFlow({ agentDir: dir, flow: "gate-only", approve: "deny" });
		assert.equal(r.completed, false);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

test('approve: "auto" lets gates through', async () => {
	const dir = await agentWith({ "gate-only.yaml": GATE_ONLY });
	try {
		const r = await runFlow({ agentDir: dir, flow: "gate-only", approve: "auto" });
		assert.equal(r.completed, true);
		assert.equal(r.steps.length, 0); // a gate produces no step result
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

test("a custom approver receives the message and the step's channel", async () => {
	const dir = await agentWith({ "gate-only.yaml": GATE_ONLY });
	const seen: { message: string; channel?: string }[] = [];
	try {
		const r = await runFlow({
			agentDir: dir,
			flow: "gate-only",
			approve: async (message, channel) => { seen.push({ message, channel }); return true; },
		});
		assert.equal(r.completed, true);
		assert.equal(seen.length, 1);
		assert.equal(seen[0].channel, "telegram");
		assert.match(seen[0].message, /Approval Required: Continue\?/);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

// ── Result shape ───────────────────────────────────────────────────────

test("result carries usage totals even when nothing ran", async () => {
	const dir = await agentWith({ "gate-only.yaml": GATE_ONLY });
	try {
		const r = await runFlow({ agentDir: dir, flow: "gate-only", approve: "auto" });
		assert.deepEqual(r.usage, []);
		assert.equal(r.totalCostUsd, 0);
		assert.equal(r.totalTokens, 0);
		assert.equal(r.flow, "gate-only");
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

test("progress events are forwarded through the wrapper", async () => {
	const dir = await agentWith({ "gate-only.yaml": GATE_ONLY });
	const events: string[] = [];
	try {
		await runFlow({
			agentDir: dir,
			flow: "gate-only",
			approve: "auto",
			onProgress: (e) => events.push(e.type),
		});
		assert.deepEqual(events, ["flow_start", "approval_requested", "approval_resolved", "flow_done"]);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});
