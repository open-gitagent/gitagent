// Tests for the gap-hardening batch:
//   G1  — loader.cloneGitRepo uses argv (no shell) → no load-time RCE
//   G25 — mutating tools run sequentially (executionMode) so writes don't race
//
// Follows the repo convention: import compiled output from ../dist (run
// `npm run build` first).

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, existsSync, rmSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

let cloneGitRepo: typeof import("../dist/loader.js").cloneGitRepo;
let createBuiltinTools: typeof import("../dist/tools/index.js").createBuiltinTools;
let buildTool: typeof import("../dist/tool-factory.js").buildTool;
let toAgentTool: typeof import("../dist/tool-utils.js").toAgentTool;
let runHooks: typeof import("../dist/hooks.js").runHooks;
let executeScheduledJob: typeof import("../dist/schedule-runner.js").executeScheduledJob;
let createCliTool: typeof import("../dist/tools/cli.js").createCliTool;

before(async () => {
	({ cloneGitRepo } = await import("../dist/loader.js"));
	({ createBuiltinTools } = await import("../dist/tools/index.js"));
	({ buildTool } = await import("../dist/tool-factory.js"));
	({ toAgentTool } = await import("../dist/tool-utils.js"));
	({ runHooks } = await import("../dist/hooks.js"));
	({ executeScheduledJob } = await import("../dist/schedule-runner.js"));
	({ createCliTool } = await import("../dist/tools/cli.js"));
});

// ── G1: no shell injection via clone URL/branch ─────────────────────────

describe("G1 cloneGitRepo (argv, no shell)", () => {
	function sentinelPath() {
		return join(tmpdir(), `gitagent-rce-${process.pid}-${Math.random().toString(36).slice(2)}`);
	}
	function freshDest() {
		const d = mkdtempSync(join(tmpdir(), "gitagent-clone-"));
		rmSync(d, { recursive: true, force: true }); // git clone needs a non-existent dest
		return d;
	}

	for (const payload of [
		(s: string) => `$(touch ${s})`,
		(s: string) => `x; touch ${s}`,
		(s: string) => `\`touch ${s}\``,
		(s: string) => `x&&touch ${s}`,
	]) {
		it(`does not shell-execute a malicious URL: ${payload("SENTINEL")}`, () => {
			const sentinel = sentinelPath();
			const ok = cloneGitRepo(payload(sentinel), freshDest(), { cwd: tmpdir() });
			assert.equal(ok, false, "bogus URL should fail to clone");
			assert.equal(existsSync(sentinel), false, "shell substitution must NOT have run");
		});
	}

	it("does not shell-execute a malicious branch", () => {
		const sentinel = sentinelPath();
		const ok = cloneGitRepo("/nonexistent/repo", freshDest(), {
			cwd: tmpdir(),
			branch: `$(touch ${sentinel})`,
		});
		assert.equal(ok, false);
		assert.equal(existsSync(sentinel), false);
	});
});

// ── G25: mutating tools sequential, read-only parallel ──────────────────

describe("G25 tool executionMode", () => {
	it("mutating built-ins are sequential; read is not", () => {
		const tools = createBuiltinTools({ dir: "/tmp", gitagentDir: "/tmp/.gitagent-test" });
		const byName = Object.fromEntries(tools.map((t) => [t.name, t]));
		for (const n of ["cli", "write", "edit", "memory"]) {
			assert.equal(byName[n].executionMode, "sequential", `${n} must be sequential`);
		}
		assert.notEqual(byName["read"].executionMode, "sequential", "read should stay parallel-safe");
	});

	it("buildTool maps isConcurrencySafe → executionMode (fail-closed)", () => {
		const def = (metadata?: any) => ({ name: "t", description: "", parameters: {}, execute: async () => "ok", metadata });
		assert.equal((buildTool(def()) as any).executionMode, "sequential");
		assert.equal((buildTool(def({ isConcurrencySafe: false })) as any).executionMode, "sequential");
		assert.equal((buildTool(def({ isConcurrencySafe: true })) as any).executionMode, "parallel");
	});

	it("SDK tools (toAgentTool) default to sequential", () => {
		const t = toAgentTool({ name: "x", description: "", inputSchema: {}, handler: async () => "ok" });
		assert.equal((t as any).executionMode, "sequential");
	});
});

// ── G20: a hook that exits before reading stdin must not crash the process ──

describe("G20 hook stdin EPIPE", () => {
	it("runHooks survives a script that exits without reading stdin", async () => {
		const dir = mkdtempSync(join(tmpdir(), "gitagent-hook-"));
		mkdirSync(join(dir, "hooks"), { recursive: true });
		writeFileSync(join(dir, "hooks", "exit0.sh"), "exit 0\n");
		// Run several times to shake out the write→EPIPE race.
		// Empty stdout is invalid JSON → fail-closed for pre_tool_use (G5).
		for (let i = 0; i < 5; i++) {
			const res = await runHooks([{ script: "exit0.sh" } as any], dir, { event: "pre_tool_use", tool: "cli", args: {} } as any);
			assert.equal(res.action, "block");
		}
		rmSync(dir, { recursive: true, force: true });
	});
});

// ── G5: safety hooks fail closed; observational hooks stay fail-open ─────

describe("G5 hooks fail-closed", () => {
	function hookDir(scripts: Record<string, string>) {
		const dir = mkdtempSync(join(tmpdir(), "gitagent-g5-"));
		mkdirSync(join(dir, "hooks"), { recursive: true });
		for (const [name, body] of Object.entries(scripts)) {
			writeFileSync(join(dir, "hooks", name), body);
		}
		return dir;
	}

	it("pre_tool_use fails closed on non-zero exit", async () => {
		const dir = hookDir({ "boom.sh": "#!/bin/sh\nexit 1\n" });
		const res = await runHooks(
			[{ script: "boom.sh" }],
			dir,
			{ event: "pre_tool_use", tool: "cli", args: { command: "echo PWNED" } },
		);
		assert.equal(res.action, "block");
		assert.match(res.reason || "", /failed|exited with code 1/i);
		rmSync(dir, { recursive: true, force: true });
	});

	it("pre_tool_use fails closed on invalid JSON", async () => {
		const dir = hookDir({ "badjson.sh": "#!/bin/sh\necho NOT_JSON\n" });
		const res = await runHooks(
			[{ script: "badjson.sh" }],
			dir,
			{ event: "pre_tool_use", tool: "cli", args: {} },
		);
		assert.equal(res.action, "block");
		assert.match(res.reason || "", /failed|invalid JSON|valid JSON/i);
		rmSync(dir, { recursive: true, force: true });
	});

	it("post_response still fails open on non-zero exit", async () => {
		const dir = hookDir({ "boom.sh": "#!/bin/sh\nexit 1\n" });
		const res = await runHooks(
			[{ script: "boom.sh" }],
			dir,
			{ event: "post_response", content: "hi" },
		);
		assert.equal(res.action, "allow");
		rmSync(dir, { recursive: true, force: true });
	});
});

// ── G21: a throwing job must not wedge the schedule "already running" ────

describe("G21 scheduler cleanup", () => {
	it("clears runningJobs even when the job throws (not stuck forever)", async () => {
		const dir = mkdtempSync(join(tmpdir(), "gitagent-sched-"));
		let runCount = 0;
		const opts: any = {
			agentDir: dir,
			runPrompt: async () => { runCount++; return "ok"; },
			// Throw on the *end* broadcast — after runPrompt, inside the try.
			broadcastToBrowsers: (m: any) => { if (m.type === "schedule_result") throw new Error("boom"); },
			appendToHistory: () => {},
		};
		const schedule: any = { id: "t1", prompt: "hi", cron: "* * * * *", mode: "repeat", enabled: true };

		// Both calls reject (end broadcast throws), but if the job were stuck the
		// second call would be skipped (resolve) and runCount would stay 1.
		await assert.rejects(() => executeScheduledJob(schedule, opts, false));
		await assert.rejects(() => executeScheduledJob(schedule, opts, false));
		assert.equal(runCount, 2, "job must run again — runningJobs was cleared in finally");
		rmSync(dir, { recursive: true, force: true });
	});
});

// ── G18: cli must not hang when a background grandchild holds the pipe ───

describe("G18 cli process-group kill", { skip: process.platform === "win32" }, () => {
	it("times out promptly instead of hanging on a background grandchild", async () => {
		const cli = createCliTool(tmpdir());
		const start = Date.now();
		// `sleep 30 &` outlives the shell and inherits the stdout pipe. Without a
		// process-group kill, 'close' never fires and this would hang forever.
		await assert.rejects(
			() => (cli as any).execute("id", { command: "sleep 30 & echo go", timeout: 2 }),
			/timed out/i,
		);
		const elapsed = Date.now() - start;
		assert.ok(elapsed < 8000, `cli should reject promptly, took ${elapsed}ms`);
	});
});
