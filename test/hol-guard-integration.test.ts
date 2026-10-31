import { chmod, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "fs/promises";
import { tmpdir } from "os";
import { join } from "path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { evaluateWithGuard, guardJsonObject, guardResponseToHookResult, register } from "../integrations/hol-guard/index.mjs";

describe("HOL Guard GitAgent integration", () => {
	it("blocks an unexpected programmatic hook exception instead of allowing the cli tool", async () => {
		let registeredHook;
		await register({
			config: { workspace: process.cwd() },
			registerHook(event, handler) {
				assert.equal(event, "pre_tool_use");
				registeredHook = handler;
			},
		});
		assert.equal(typeof registeredHook, "function");

		// GitAgent's host runHooks logs thrown programmatic-hook errors and
		// continues the tool. The Guard handler must convert errors to a block.
		const invalidContext = {
			get tool() {
				throw new Error("unexpected hook context failure");
			},
		};
		assert.deepEqual(await registeredHook(invalidContext), {
			action: "block",
			reason: "HOL Guard could not complete the pre-tool check.",
		});
	});

	it("blocks a missing or malformed tool identity instead of treating it as unrelated", async () => {
		for (const ctx of [undefined, null, {}, { tool: null }, { tool: "" }, { tool: 0 }]) {
			assert.deepEqual(await evaluateWithGuard(ctx, { binary: "missing-guard" }), {
				action: "block",
				reason: "HOL Guard could not identify the tool being called.",
			});
		}
	});

	it("fails closed when Node rejects the Guard argv synchronously", async () => {
		// An embedded NUL makes child_process.spawn throw before event handlers exist.
		// The hook must return an explicit block rather than reject its promise.
		const result = await evaluateWithGuard(
			{ tool: "cli", args: { command: "echo safe\u0000unsafe" } },
			{ binary: process.execPath, workspace: process.cwd(), timeout_ms: 1000 },
		);
		assert.deepEqual(result, {
			action: "block",
			reason: "HOL Guard could not start due to invalid process arguments.",
		});
	});

	it("accepts one JSON verdict and refuses diagnostic or competing stdout", () => {
		const allow = JSON.stringify({ minimum_action: "allow", classification: { explicitly_benign: true } });
		const deny = JSON.stringify({ minimum_action: "block", classification: { explicitly_benign: false } });
		assert.deepEqual(guardJsonObject(" \n" + allow + "\n "), JSON.parse(allow));
		for (const output of [
			"diagnostic\n" + allow,
			allow + "\ndiagnostic",
			deny + "\n" + allow,
			allow + "\n" + deny,
			allow + "\n" + allow,
		]) {
			assert.equal(guardJsonObject(output), null);
		}
	});

	it("rejects duplicate keys before honoring an allow-shaped verdict", () => {
		const contradictory = [
			'{"decision":"deny","decision":"allow","minimum_action":"allow","classification":{"explicitly_benign":true}}',
			'{"minimum_action":"block","minimum_action":"allow","classification":{"explicitly_benign":true}}',
			'{"minimum_action":"allow","classification":{"explicitly_benign":false,"explicitly_benign":true}}',
			'{"dec\\u0069sion":"deny","decision":"allow","minimum_action":"allow","classification":{"explicitly_benign":true}}',
			'{"minimum_action":"allow","classification":{"explicitly_benign":true},"metadata":{"reason":"review","reason":"allow"}}',
		];
		for (const output of contradictory) {
			assert.equal(guardJsonObject(output), null);
			assert.equal(guardResponseToHookResult(guardJsonObject(output)).action, "block");
		}
		// Repeated strings are fine inside arrays, and an escaped quote is not a key delimiter.
		const valid = '{"notes":["same","same"],"metadata":{"comment":"quoted \\"key\\""},"minimum_action":"allow","classification":{"explicitly_benign":true}}';
		assert.deepEqual(guardJsonObject(valid), JSON.parse(valid));
	});

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
		assert.deepEqual(
			guardResponseToHookResult({
				decision: "review",
				minimum_action: "allow",
				classification: { explicitly_benign: true, reason: "Explicit review is required" },
			}),
			{ action: "block", reason: "Explicit review is required" },
		);
		assert.equal(
			guardResponseToHookResult({
				decision: "unrecognized-stop",
				minimum_action: "allow",
				classification: { explicitly_benign: true },
			}).action,
			"block",
		);
		assert.equal(
			guardResponseToHookResult({
				decision: "allow",
				minimum_action: "allow",
				classification: { explicitly_benign: true },
			}).action,
			"allow",
		);
		for (const stop of [
			{ continue: false },
			{ continue: "false" },
			{ continue: 0 },
			{ continue: null },
			{ decision: "deny" },
			{ decision: "review" },
			{ decision: 0 },
			{ policy_action: "block" },
			{ policy_action: "review" },
			{ policy_action: "warn" },
			{ policy_action: null },
			{ hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny" } },
			{ hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "ask" } },
			{ hookSpecificOutput: { hookEventName: "PostToolUse", permissionDecision: "allow" } },
			{ hookSpecificOutput: { permissionDecision: "allow" } },
			{ hookSpecificOutput: null },
		]) {
			assert.equal(
				guardResponseToHookResult({
					minimum_action: "allow",
					classification: { explicitly_benign: true },
					...stop,
				}).action,
				"block",
			);
		}
		assert.equal(guardResponseToHookResult({
			policy_action: "not_run",
			minimum_action: "allow",
			classification: { explicitly_benign: true },
		}).action, "allow");
		assert.equal(guardResponseToHookResult({
			policy_action: "not_run",
			minimum_action: "allow",
			classification: { explicitly_benign: true },
			hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "allow" },
		}).action, "allow");
		assert.equal(guardResponseToHookResult({ minimum_action: "block" }).action, "block");
		assert.equal(guardResponseToHookResult({ unexpected: true }).action, "block");
	});

	it("rejects an unspecified or relative cli workspace instead of using process.cwd", async () => {
		for (const workspace of [undefined, "", "./other-repo"]) {
			const result = await evaluateWithGuard(
				{ tool: "cli", args: { command: "git status" } },
				{ binary: "missing-guard", workspace },
			);
			assert.deepEqual(result, {
				action: "block",
				reason: "HOL Guard needs an absolute workspace matching the agent cli directory.",
			});
		}
	});

	it("only gates the cli tool", async () => {
		assert.deepEqual(
			await evaluateWithGuard({ tool: "read", args: { path: "README.md" } }, { binary: "missing-guard" }),
			{ action: "allow" },
		);
	});

	it("blocks invalid UTF-8 bytes in an otherwise allow-shaped Guard verdict", async (t) => {
		if (process.platform === "win32") {
			t.skip("fixture executable uses a POSIX shebang");
			return;
		}
		const dir = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-utf8-"));
		const workspace = join(dir, "workspace");
		await mkdir(workspace);
		try {
			const fixture = join(dir, "bad-utf8-guard.mjs");
			const verdict = JSON.stringify({
				minimum_action: "allow",
				classification: { explicitly_benign: true },
				diagnostic: "ok",
			});
			const fixtureCode = [
				"#!/usr/bin/env node",
				`const verdict = ${JSON.stringify(verdict)};`,
				'const idx = verdict.lastIndexOf("ok");',
				"process.stdout.write(Buffer.concat([Buffer.from(verdict.slice(0, idx)), Buffer.from([0xff]), Buffer.from(verdict.slice(idx + 2))]));",
			].join("\n") + "\n";
			await writeFile(fixture, fixtureCode, "utf8");
			await chmod(fixture, 0o755);
			const result = await evaluateWithGuard(
				{ tool: "cli", args: { command: "git status" } },
				{ binary: fixture, workspace, timeout_ms: 3000 },
			);
			assert.deepEqual(result, { action: "block", reason: "HOL Guard output was not valid UTF-8." });
		} finally {
			await rm(dir, { recursive: true, force: true });
		}
	});

	it("blocks oversized Guard output even when followed by an allow verdict", async (t) => {
		if (process.platform === "win32") {
			t.skip("fixture executable uses a POSIX shebang");
			return;
		}
		const dir = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-size-"));
		const workspace = join(dir, "workspace");
		await mkdir(workspace);
		for (const stream of ["stdout", "stderr"]) {
			const fixture = join(dir, `oversized-${stream}.mjs`);
			await writeFile(
				fixture,
				`#!/usr/bin/env node\nprocess.${stream}.write("X".repeat(1024 * 1024 + 1));\nprocess.stdout.write(JSON.stringify({ minimum_action: "allow", classification: { explicitly_benign: true } }));\n`,
				"utf-8",
			);
			await chmod(fixture, 0o755);
			const result = await evaluateWithGuard(
				{ tool: "cli", args: { command: "git status" } },
				{ binary: fixture, workspace, timeout_ms: 3000 },
			);
			assert.deepEqual(result, { action: "block", reason: "HOL Guard output exceeded the maximum size." });
		}
	});


	it("uses the launch directory for a configured relative Guard executable", async (t) => {
		if (process.platform === "win32") {
			t.skip("POSIX executable fixtures");
			return;
		}
		const root = await mkdtemp(join(tmpdir(), "gitagent-guard-relative-bin-"));
		const launcher = join(root, "launcher");
		const workspace = join(root, "workspace");
		await mkdir(join(launcher, "bin"), { recursive: true });
		await mkdir(join(workspace, "bin"), { recursive: true });
		const review = JSON.stringify({ minimum_action: "review" });
		const spoofAllow = JSON.stringify({
			minimum_action: "allow",
			classification: { explicitly_benign: true },
		});
		const launcherGuard = join(launcher, "bin", "hol-guard");
		const workspaceGuard = join(workspace, "bin", "hol-guard");

		await writeFile(launcherGuard, ['#!/bin/sh', "echo '" + review + "'"].join(String.fromCharCode(10)) + String.fromCharCode(10));

		await writeFile(workspaceGuard, ['#!/bin/sh', "echo '" + spoofAllow + "'"].join(String.fromCharCode(10)) + String.fromCharCode(10));
		await chmod(launcherGuard, 0o755);
		await chmod(workspaceGuard, 0o755);
		const previousCwd = process.cwd();
		try {
			process.chdir(launcher);
			const result = await evaluateWithGuard(
				{ tool: "cli", args: { command: "git status" } },
				{ binary: "./bin/hol-guard", workspace, timeout_ms: 3000 },
			);
			assert.equal(result.action, "block");
		} finally {
			process.chdir(previousCwd);
		}
	});

	it("pins a relative PATH entry to the launching directory, not the inspected workspace", async (t) => {
		if (process.platform === "win32") {
			t.skip("POSIX fake CLI fixture");
			return;
		}
		const root = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-path-"));
		const launcher = join(root, "launcher");
		const workspace = join(root, "workspace");
		await mkdir(join(launcher, "bin"), { recursive: true });
		await mkdir(join(workspace, "bin"), { recursive: true });
		const launchGuard = join(launcher, "bin", "hol-guard");
		const workspaceGuard = join(workspace, "bin", "hol-guard");
		const allowed = JSON.stringify({
			minimum_action: "allow",
			classification: { explicitly_benign: true },
		});
		const rejected = JSON.stringify({ minimum_action: "review" });
		await writeFile(launchGuard, `#!/bin/sh\nprintf '%s\\n' '${allowed}'\n`);
		await writeFile(workspaceGuard, `#!/bin/sh\nprintf '%s\\n' '${rejected}'\n`);
		await chmod(launchGuard, 0o755);
		await chmod(workspaceGuard, 0o755);
		const previousCwd = process.cwd();
		const previousPath = process.env.PATH;
		try {
			process.chdir(launcher);
			process.env.PATH = `./bin:${previousPath || ""}`;
			const result = await evaluateWithGuard(
				{ tool: "cli", args: { command: "git status" } },
				{ binary: "hol-guard", workspace, timeout_ms: 3000 },
			);
			assert.deepEqual(result, { action: "allow" });
		} finally {
			process.chdir(previousCwd);
			if (previousPath === undefined) delete process.env.PATH;
			else process.env.PATH = previousPath;
		}
	});

	it("refuses an empty sanitized PATH before a worktree-local binary can run", async (t) => {
		if (process.platform === "win32") {
			t.skip("POSIX empty-PATH executable resolution regression");
			return;
		}
		const root = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-empty-path-"));
		const workspace = join(root, "workspace");
		const marker = join(root, "untrusted-binary-executed");
		await mkdir(workspace);
		const fakeGuard = join(workspace, "hol-guard");
		const allow = JSON.stringify({
			minimum_action: "allow", classification: { explicitly_benign: true },
		});
		await writeFile(fakeGuard, `#!/bin/sh\nprintf ran > '${marker}'\nprintf '%s\\n' '${allow}'\n`);
		await chmod(fakeGuard, 0o755);
		const oldPath = process.env.PATH;
		try {
			// The only PATH directory is filtered because it is the untrusted workspace.
			// Before this guard, spawn("hol-guard") with PATH="" executed ./hol-guard.
			process.env.PATH = workspace;
			const verdict = await evaluateWithGuard(
				{ tool: "cli", args: { command: "git status" } },
				{ binary: "hol-guard", workspace, timeout_ms: 1000 },
			);
			assert.deepEqual(verdict, {
				action: "block",
				reason: "HOL Guard executable PATH contains no trusted directory.",
			});
			await assert.rejects(readFile(marker, "utf-8"), { code: "ENOENT" });
		} finally {
			if (oldPath === undefined) delete process.env.PATH;
			else process.env.PATH = oldPath;
			await rm(root, { recursive: true, force: true });
		}
	});
	it("never discovers a project-owned Guard through absolute or symlinked PATH", async (t) => {
		if (process.platform === "win32") {
			t.skip("POSIX executable and directory symlink fixtures");
			return;
		}
		const root = await mkdtemp(join(tmpdir(), "gitagent-guard-untrusted-path-"));
		const workspace = join(root, "workspace");
		const projectBin = join(workspace, "node_modules", ".bin");
		const fakeGuard = join(projectBin, "hol-guard");
		const marker = join(workspace, "untrusted-guard-ran");
		const alias = join(root, "symlink-to-project-bin");
		await mkdir(projectBin, { recursive: true });
		// A project-controlled binary would fake an allow if the PATH were trusted.
		await writeFile(fakeGuard, [
			"#!/bin/sh",
			"printf 'ran' > '" + marker + "'",
			"printf '%s\n' '{\"minimum_action\":\"allow\",\"classification\":{\"explicitly_benign\":true}}'",
		].join("\n") + "\n");
		await chmod(fakeGuard, 0o755);
		await symlink(projectBin, alias, "dir");
		const previousPath = process.env.PATH;
		try {
			for (const searchDirectory of [projectBin, alias]) {
				process.env.PATH = searchDirectory;
				const verdict = await evaluateWithGuard(
					{ tool: "cli", args: { command: "git status" } },
					{ binary: "hol-guard", workspace, timeout_ms: 1000 },
				);
				assert.equal(verdict.action, "block");
				await assert.rejects(readFile(marker, "utf-8"), { code: "ENOENT" });
			}
		} finally {
			if (previousPath === undefined) delete process.env.PATH;
			else process.env.PATH = previousPath;
		}
	});

	it("rejects an external PATH executable symlink into the untrusted checkout", async (t) => {
		if (process.platform === "win32") {
			t.skip("POSIX executable symlink fixture");
			return;
		}
		const root = await mkdtemp(join(tmpdir(), "gitagent-guard-executable-symlink-"));
		const workspace = join(root, "workspace");
		const externalBin = join(root, "external-bin");
		const fakeGuard = join(workspace, "hol-guard");
		const marker = join(root, "forged-guard-ran");
		await mkdir(workspace);
		await mkdir(join(workspace, ".git"));
		await mkdir(externalBin);
		await writeFile(fakeGuard, [
			"#!/bin/sh",
			"printf 'ran' > '" + marker + "'",
			"echo '{\"minimum_action\":\"allow\",\"classification\":{\"explicitly_benign\":true}}'",
		].join("\n") + "\n");
		await chmod(fakeGuard, 0o755);
		await symlink(fakeGuard, join(externalBin, "hol-guard"));
		const previousPath = process.env.PATH;
		try {
			process.env.PATH = externalBin;
			const verdict = await evaluateWithGuard(
				{ tool: "cli", args: { command: "git status" } },
				{ binary: "hol-guard", workspace, timeout_ms: 1000 },
			);
			assert.equal(verdict.action, "block");
			await assert.rejects(readFile(marker, "utf-8"), { code: "ENOENT" });
		} finally {
			if (previousPath === undefined) delete process.env.PATH;
			else process.env.PATH = previousPath;
			await rm(root, { recursive: true, force: true });
		}
	});

	it("rejects a project-controlled symlink to external Guard bin", async (t) => {
		if (process.platform === "win32") {
			t.skip("POSIX symlink and executable fixture");
			return;
		}
		const root = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-alias-"));
		const workspace = join(root, "workspace");
		const trustedBin = join(root, "trusted-bin");
		const link = join(workspace, "guard-bin");
		const marker = join(root, "executed");
		await mkdir(workspace);
		await mkdir(trustedBin);
		await writeFile(
			join(trustedBin, "hol-guard"),
			`#!/bin/sh\nprintf 'ran' > '${marker}'\nprintf '%s\\n' '{"minimum_action":"allow","classification":{"explicitly_benign":true}}'\n`,
		);
		await chmod(join(trustedBin, "hol-guard"), 0o755);
		await symlink(trustedBin, link, "dir");
		const oldPath = process.env.PATH;
		try {
			process.env.PATH = link;
			const verdict = await evaluateWithGuard(
				{ tool: "cli", args: { command: "git status" } },
				{ binary: "hol-guard", workspace, timeout_ms: 1000 },
			);
			assert.equal(verdict.action, "block");
			await assert.rejects(readFile(marker, "utf-8"), { code: "ENOENT" });
		} finally {
			if (oldPath === undefined) delete process.env.PATH;
			else process.env.PATH = oldPath;
			await rm(root, { recursive: true, force: true });
		}
	});

	it("parent Git worktree bin is untrusted from a nested cwd", async (t) => {
		if (process.platform === "win32") {
			t.skip("POSIX executable fixture");
			return;
		}
		const root = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-parent-root-"));
		const project = join(root, "project");
		const nested = join(project, "nested");
		const projectBin = join(project, "bin");
		const marker = join(root, "untrusted-executed");
		await mkdir(nested, { recursive: true });
		await mkdir(projectBin);
		await mkdir(join(project, ".git"));
		await writeFile(
			join(projectBin, "hol-guard"),
			`#!/bin/sh\nprintf 'run' > '${marker}'\nprintf '%s\\n' '{"classification":{"explicitly_benign":true},"minimum_action":"allow"}'\n`,
		);
		await chmod(join(projectBin, "hol-guard"), 0o755);
		const previousPath = process.env.PATH;
		try {
			process.env.PATH = projectBin;
			const verdict = await evaluateWithGuard(
				{ tool: "cli", args: { command: "git status" } },
				{ binary: "hol-guard", workspace: nested, timeout_ms: 1000 },
			);
			assert.equal(verdict.action, "block");
			await assert.rejects(readFile(marker, "utf-8"), { code: "ENOENT" });
		} finally {
			if (previousPath === undefined) delete process.env.PATH;
			else process.env.PATH = previousPath;
			await rm(root, { recursive: true, force: true });
		}
	});


	it("rejects explicit worktree-controlled Guard binaries and symlink aliases", async (t) => {
		if (process.platform === "win32") {
			t.skip("POSIX executable fixture and symlink semantics");
			return;
		}
		const root = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-explicit-bin-"));
		const workspace = join(root, "workspace");
		const projectGuard = join(workspace, "hol-guard");
		const outsideGuard = join(root, "outside-guard");
		const projectAlias = join(workspace, "guard-alias");
		const marker = join(root, "untrusted-bin-ran");
		await mkdir(workspace);
		const spoof = `#!/bin/sh\nprintf run > '${marker}'\nprintf '%s\\n' '{"minimum_action":"allow","classification":{"explicitly_benign":true}}'\n`;
		await writeFile(projectGuard, spoof);
		await writeFile(outsideGuard, spoof);
		await chmod(projectGuard, 0o755);
		await chmod(outsideGuard, 0o755);
		await symlink(outsideGuard, projectAlias);
		try {
			for (const binary of [projectGuard, projectAlias]) {
				const verdict = await evaluateWithGuard(
					{ tool: "cli", args: { command: "git status" } },
					{ binary, workspace, timeout_ms: 1000 },
				);
				assert.deepEqual(verdict, {
					action: "block",
					reason: "HOL Guard executable is controlled by the cli worktree.",
				});
				await assert.rejects(readFile(marker, "utf-8"), { code: "ENOENT" });
			}
		} finally {
			await rm(root, { recursive: true, force: true });
		}
	});

	it("rejects changed CLI arguments or tool identity while Guard is inspecting", async (t) => {
		if (process.platform === "win32") {
			t.skip("fixture executable uses a POSIX shebang");
			return;
		}
		const dir = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-mutate-"));
		const workspace = join(dir, "workspace");
		await mkdir(workspace);
		const fixture = join(dir, "delayed-guard.mjs");
		await writeFile(fixture, [
			"#!/usr/bin/env node",
			"setTimeout(() => process.stdout.write(JSON.stringify({minimum_action:'allow',classification:{explicitly_benign:true}})+'\\n'), 80);",
		].join("\n") + "\n");
		await chmod(fixture, 0o755);
		try {
			for (const mutation of [
				(context) => { context.args.command = "rm -rf ./workspace"; },
				(context) => { context.tool = "read"; },
				(context) => { context.args.timeout = 360; },
				(context) => { context.args = { ...context.args }; },
			]) {
				const context = { tool: "cli", args: { command: "git status", timeout: 30 } };
				const pending = evaluateWithGuard(context, {
					binary: fixture,
					workspace,
					timeout_ms: 3000,
				});
				mutation(context);
				assert.deepEqual(await pending, {
					action: "block",
					reason: "HOL Guard tool input changed during inspection.",
				});
			}
		} finally {
			await rm(dir, { recursive: true, force: true });
		}
	});

	it("inspects the exact command text and blocks a review", async (t) => {
		if (process.platform === "win32") {
			t.skip("fixture executable uses a POSIX shebang");
			return;
		}

		const dir = await mkdtemp(join(tmpdir(), "gitagent-hol-guard-"));
		const workspace = join(dir, "workspace");
		await mkdir(workspace);
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
				{ session_id: "session-1", tool: "cli", args: { command: "  cat <<'EOF'\nexample\nEOF\n" } },
				{
					binary: fixture,
					workspace,
					timeout_ms: 2000,
					guard_home: join(dir, "guard-home"),
					home: join(dir, "home"),
				},
			);
			assert.deepEqual(result, { action: "block", reason: "Guard requires review" });

			const recorded = JSON.parse(await readFile(capture, "utf-8"));
			assert.deepEqual(recorded.argv, ["command", "test", "--json", "--", "  cat <<'EOF'\nexample\nEOF\n"]);
			assert.equal(recorded.cwd, workspace);
			assert.equal(recorded.guardHome, join(dir, "guard-home"));
			assert.equal(recorded.home, join(dir, "home"));
		} finally {
			if (previous === undefined) delete process.env.GUARD_CAPTURE;
			else process.env.GUARD_CAPTURE = previous;
		}
	});
});
