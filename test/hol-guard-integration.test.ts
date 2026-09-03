import { chmod, mkdtemp, readFile, writeFile } from "fs/promises";
import { tmpdir } from "os";
import { join } from "path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { evaluateWithGuard, guardResponseToHookResult } from "../integrations/hol-guard/index.mjs";

describe("HOL Guard GitAgent integration", () => {
	it("maps Guard decisions to GitAgent hook results", () => {
		assert.deepEqual(
			guardResponseToHookResult({ hookSpecificOutput: { permissionDecision: "allow" } }),
			{ action: "allow" },
		);
		assert.deepEqual(
			guardResponseToHookResult({
				hookSpecificOutput: {
					permissionDecision: "deny",
					permissionDecisionReason: "blocked by guard",
				},
			}),
			{ action: "block", reason: "blocked by guard" },
		);
		assert.equal(
			guardResponseToHookResult({ hookSpecificOutput: { permissionDecision: "ask" } }).action,
			"block",
		);
		assert.equal(guardResponseToHookResult({ unexpected: true }).action, "block");
	});

	it("only gates the cli tool", async () => {
		assert.deepEqual(
			await evaluateWithGuard({ tool: "read", args: { path: "README.md" } }, { binary: "missing-guard" }),
			{ action: "allow" },
		);
	});

	it("invokes HOL Guard with the command payload and blocks a deny", async (t) => {
		if (process.platform === "win32") {
			t.skip("fixture executable uses a POSIX shebang");
			return;
		}

		const dir = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-"));
		const capture = join(dir, "capture.json");
		const fixture = join(dir, "hol-guard-fixture.mjs");
		await writeFile(
			fixture,
			`#!/usr/bin/env node\nimport { writeFileSync } from "node:fs";\nlet input = "";\nfor await (const chunk of process.stdin) input += chunk;\nwriteFileSync(process.env.GUARD_CAPTURE, JSON.stringify({ argv: process.argv.slice(2), input: JSON.parse(input) }));\nprocess.stdout.write(JSON.stringify({ hookSpecificOutput: { permissionDecision: "deny", permissionDecisionReason: "Guard blocked the command" } }) + "\\n");\n`,
			"utf-8",
		);
		await chmod(fixture, 0o755);

		const previous = process.env.GUARD_CAPTURE;
		process.env.GUARD_CAPTURE = capture;
		try {
			const result = await evaluateWithGuard(
				{ session_id: "session-1", tool: "cli", args: { command: "rm -rf ./build" } },
				{ binary: fixture, workspace: dir, timeout_ms: 2000 },
			);
			assert.deepEqual(result, { action: "block", reason: "Guard blocked the command" });

			const recorded = JSON.parse(await readFile(capture, "utf-8"));
			assert.deepEqual(recorded.argv.slice(0, 4), ["guard", "hook", "--harness", "codex"]);
			assert.ok(recorded.argv.includes("--json"));
			assert.equal(recorded.input.hook_event_name, "PreToolUse");
			assert.equal(recorded.input.tool_name, "Bash");
			assert.equal(recorded.input.tool_input.command, "rm -rf ./build");
		} finally {
			if (previous === undefined) delete process.env.GUARD_CAPTURE;
			else process.env.GUARD_CAPTURE = previous;
		}
	});
});
