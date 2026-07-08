// Unit tests for src/permissions.ts and src/tools/exit-plan-mode.ts

import test from "node:test";
import assert from "node:assert/strict";

import {
	evaluatePermission,
	describeToolCall,
	wrapToolWithPermissions,
	normalizeRules,
	type PermissionState,
	type PermissionMode,
} from "../src/permissions.ts";
import { createExitPlanModeTool } from "../src/tools/exit-plan-mode.ts";

// ── Helpers ────────────────────────────────────────────────────────────

function fakeTool(name: string, opts: { isReadOnly?: boolean; exec?: () => Promise<any> } = {}): any {
	return {
		name,
		label: name,
		description: name,
		parameters: {},
		metadata: opts.isReadOnly !== undefined ? { isReadOnly: opts.isReadOnly } : undefined,
		execute: async () =>
			opts.exec ? opts.exec() : { content: [{ type: "text", text: "ok" }], details: undefined },
	};
}

function state(partial: Partial<PermissionState> = {}): PermissionState {
	return {
		mode: "default",
		rules: normalizeRules(),
		sessionId: "s",
		agentName: "a",
		planDeferred: null,
		...partial,
	};
}

function desc(name: string, args: any) {
	return describeToolCall(fakeTool(name), args);
}

// ── describeToolCall classification ────────────────────────────────────

test("describeToolCall classifies built-in tools", () => {
	assert.equal(desc("read", { path: "a.ts" }).mutates, false);
	assert.equal(desc("write", { path: "a.ts" }).mutates, true);
	assert.equal(desc("cli", { command: "ls" }).mutates, true);
	assert.equal(desc("memory", { action: "load" }).mutates, false);
	assert.equal(desc("memory", { action: "save" }).mutates, true);
	assert.equal(desc("skill_learner", { action: "evaluate" }).mutates, false);
	assert.equal(desc("skill_learner", { action: "crystallize" }).mutates, true);
});

test("describeToolCall falls back to metadata for unknown tools", () => {
	const ro = describeToolCall(fakeTool("custom_ro", { isReadOnly: true }), { q: "x" });
	assert.equal(ro.mutates, false);
	const rw = describeToolCall(fakeTool("custom_rw", { isReadOnly: false }), { q: "x" });
	assert.equal(rw.mutates, true);
});

// ── evaluatePermission across modes ────────────────────────────────────

test("bypassPermissions allows everything", async () => {
	const d = await evaluatePermission(desc("write", { path: "x" }), state({ mode: "bypassPermissions" }));
	assert.equal(d.behavior, "allow");
});

test("default mode allows read-only, asks on unmatched mutating (fail-closed deny)", async () => {
	const read = await evaluatePermission(desc("read", { path: "x" }), state());
	assert.equal(read.behavior, "allow");
	// No canUseTool → fail-closed deny.
	const write = await evaluatePermission(desc("write", { path: "x" }), state());
	assert.equal(write.behavior, "deny");
});

test("acceptEdits auto-allows write/edit but still asks for cli", async () => {
	assert.equal((await evaluatePermission(desc("write", { path: "x" }), state({ mode: "acceptEdits" }))).behavior, "allow");
	assert.equal((await evaluatePermission(desc("edit", { path: "x" }), state({ mode: "acceptEdits" }))).behavior, "allow");
	assert.equal((await evaluatePermission(desc("cli", { command: "rm x" }), state({ mode: "acceptEdits" }))).behavior, "deny");
});

test("plan mode denies mutating, allows read-only", async () => {
	assert.equal((await evaluatePermission(desc("read", { path: "x" }), state({ mode: "plan" }))).behavior, "allow");
	assert.equal((await evaluatePermission(desc("grep", { pattern: "x" }), state({ mode: "plan" }))).behavior, "allow");
	const w = await evaluatePermission(desc("write", { path: "x" }), state({ mode: "plan" }));
	assert.equal(w.behavior, "deny");
	assert.match(w.message ?? "", /plan mode/i);
});

// ── Rules ──────────────────────────────────────────────────────────────

test("deny rule wins; allow rule permits; Bash alias maps to cli", async () => {
	const deny = await evaluatePermission(
		desc("cli", { command: "rm -rf /" }),
		state({ rules: normalizeRules({ deny: ["Bash(rm *)"] }) }),
	);
	assert.equal(deny.behavior, "deny");

	const allow = await evaluatePermission(
		desc("cli", { command: "git status" }),
		state({ rules: normalizeRules({ allow: ["Bash(git status)"] }) }),
	);
	assert.equal(allow.behavior, "allow");
});

test("glob patterns match (Write(src/**), :* prefix)", async () => {
	const inSrc = await evaluatePermission(
		desc("write", { path: "src/index.ts" }),
		state({ rules: normalizeRules({ allow: ["Write(src/**)"] }) }),
	);
	assert.equal(inSrc.behavior, "allow");

	const prefixed = await evaluatePermission(
		desc("cli", { command: "git push origin main" }),
		state({ rules: normalizeRules({ allow: ["Bash(git push:*)"] }) }),
	);
	assert.equal(prefixed.behavior, "allow");
});

test("canUseTool resolves ask decisions", async () => {
	let seen = "";
	const s = state({
		canUseTool: (name) => {
			seen = name;
			return { behavior: "allow" };
		},
	});
	const d = await evaluatePermission(desc("write", { path: "x" }), s);
	assert.equal(d.behavior, "allow");
	assert.equal(seen, "write");
});

// ── wrapToolWithPermissions ────────────────────────────────────────────

test("wrapper denies (throws) in plan mode and fires onDenied", async () => {
	let denied: string | null = null;
	const wrapped = wrapToolWithPermissions(fakeTool("write"), state({ mode: "plan" }), (n) => {
		denied = n;
	});
	await assert.rejects(() => wrapped.execute("id", { path: "x", content: "y" }), /plan mode/i);
	assert.equal(denied, "write");
});

test("wrapper allows read-only in plan mode", async () => {
	const wrapped = wrapToolWithPermissions(fakeTool("read"), state({ mode: "plan" }), () => {});
	const r = await wrapped.execute("id", { path: "x" });
	assert.equal(r.content[0].text, "ok");
});

test("wrapper never gates exit_plan_mode", async () => {
	const tool = fakeTool("exit_plan_mode");
	const wrapped = wrapToolWithPermissions(tool, state({ mode: "plan" }), () => {});
	assert.equal(wrapped, tool);
});

// ── exit_plan_mode tool ────────────────────────────────────────────────

test("exit_plan_mode emits plan, blocks, then flips mode on approval", async () => {
	const s = state({ mode: "plan" });
	let emitted = "";
	const tool = createExitPlanModeTool(s, (p) => {
		emitted = p;
	});

	const resultP = tool.execute("id", { plan: "do the thing" });
	// Let the handler register the deferred.
	await Promise.resolve();
	assert.equal(emitted, "do the thing");
	assert.ok(s.planDeferred, "deferred should be set while blocking");

	s.planDeferred!.resolve({ approved: true, nextMode: "acceptEdits" as PermissionMode });
	const result = await resultP;
	assert.match(result.content[0].text, /approved/i);
	assert.equal(s.mode, "acceptEdits");
	assert.equal(s.planDeferred, null);
});

test("exit_plan_mode stays in plan mode on rejection", async () => {
	const s = state({ mode: "plan" });
	const tool = createExitPlanModeTool(s, () => {});
	const resultP = tool.execute("id", { plan: "x" });
	await Promise.resolve();
	s.planDeferred!.resolve({ approved: false, feedback: "needs more detail" });
	const result = await resultP;
	assert.match(result.content[0].text, /rejected/i);
	assert.match(result.content[0].text, /needs more detail/);
	assert.equal(s.mode, "plan");
});
