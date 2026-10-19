import { chmod, mkdtemp, readFile, writeFile } from "fs/promises";
import { tmpdir } from "os";
import { join } from "path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { evaluateWithGuard, guardResponseToHookResult } from "../integrations/hol-guard/index.mjs";

describe("HOL Guard GitAgent integration", () => {
	it("maps Guard command floors to GitAgent hook results", () => {
		assert.deepEqual(
			guardResponseToHookResult({
				minimum_action: "allow",
				classification: { explicitly_benign: true },
			}),
			{ action: "allow" },
		);
		assert.equal(
			guardResponseToHookResult({
				minimum_action: "allow",
				classification: { explicitly_benign: false },
			}).action,
			"block",
		);
		assert.equal(
			guardResponseToHookResult({
				minimum_action: "monitor",
				classification: { explicitly_benign: true },
			}).action,
			"block",
		);
		assert.deepEqual(
			guardResponseToHookResult({
				minimum_action: "review",
				classification: { reason: "Guard requires review" },
			}),
			{ action: "block", reason: "Guard requires review" },
		);
		assert.equal(guardResponseToHookResult({ minimum_action: "block" }).action, "block");
		assert.equal(guardResponseToHookResult({ unexpected: true }).action, "block");
	});

	it("only gates the cli tool", async () => {
		assert.deepEqual(
			await evaluateWithGuard({ tool: "read", args: { path: "README.md" } }, { binary: "missing-guard" }),
			{ action: "allow" },
		);
	});

	it("invokes HOL Guard command inspection and blocks a review", async (t) => {
		if (process.platform === "win32") {
			t.skip("fixture executable uses a POSIX shebang");
			return;
		}

		const dir = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-"));
		const capture = join(dir, "capture.json");
		const fixture = join(dir, "hol-guard-fixture.mjs");
		await writeFile(
			fixture,
			`#!/usr/bin/env node\nimport { writeFileSync } from "node:fs";\nwriteFileSync(process.env.GUARD_CAPTURE, JSON.stringify({ argv: process.argv.slice(2), cwd: process.cwd(), guardHome: process.env.HOL_GUARD_HOME, home: process.env.HOME }));\nprocess.stdout.write(JSON.stringify({ minimum_action: "review", classification: { reason: "Guard requires review" } }, null, 2) + "\\n");\n`,
			"utf-8",
		);
		await chmod(fixture, 0o755);

		const previous = process.env.GUARD_CAPTURE;
		process.env.GUARD_CAPTURE = capture;
		try {
			const result = await evaluateWithGuard(
				{ session_id: "session-1", tool: "cli", args: { command: "rm -rf ./build" } },
				{
					binary: fixture,
					workspace: dir,
					timeout_ms: 2000,
					guard_home: join(dir, "guard-home"),
					home: join(dir, "home"),
				},
			);
			assert.deepEqual(result, { action: "block", reason: "Guard requires review" });

			const recorded = JSON.parse(await readFile(capture, "utf-8"));
			assert.deepEqual(recorded.argv, ["command", "test", "rm -rf ./build", "--json"]);
			assert.equal(recorded.cwd, dir);
			assert.equal(recorded.guardHome, join(dir, "guard-home"));
			assert.equal(recorded.home, join(dir, "home"));
		} finally {
			if (previous === undefined) delete process.env.GUARD_CAPTURE;
			else process.env.GUARD_CAPTURE = previous;
		}
	});
});
