// Tests for Phase D1 desktop enablers:
//   - resolveJailed() folder-jail (src/tools/shared.ts)
//   - initLocalFolderSession() local-only session (src/session.ts)

import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { tmpdir, homedir } from "node:os";
import { join } from "node:path";
import { execSync } from "node:child_process";

import { resolveJailed } from "../src/tools/shared.ts";
import { initLocalFolderSession } from "../src/session.ts";

// ── resolveJailed ──────────────────────────────────────────────────────

test("resolveJailed allows in-root paths", () => {
	assert.equal(resolveJailed("a.txt", "/root", "/root"), "/root/a.txt");
	assert.equal(resolveJailed("sub/a.txt", "/root", "/root"), "/root/sub/a.txt");
	assert.equal(resolveJailed(".", "/root", "/root"), "/root"); // the root itself
	assert.equal(resolveJailed("/root/x", "/root", "/root"), "/root/x"); // absolute in-root
});

test("resolveJailed rejects escapes", () => {
	assert.throws(() => resolveJailed("../etc/passwd", "/root", "/root"), /escapes the session folder/);
	assert.throws(() => resolveJailed("/etc/passwd", "/root", "/root"), /escapes the session folder/);
	assert.throws(() => resolveJailed("sub/../../x", "/root", "/root"), /escapes the session folder/);
	// ~ expands to the home dir, which is outside an arbitrary root.
	assert.throws(() => resolveJailed("~/.ssh/id_rsa", "/root", "/root"), /escapes the session folder/);
});

test("resolveJailed is a no-op passthrough without rootDir (legacy behavior)", () => {
	// No jail → escapes are allowed (resolves, does not throw).
	assert.equal(resolveJailed("../x", "/root"), "/x");
	assert.equal(resolveJailed("/etc/passwd", "/root"), "/etc/passwd");
	assert.equal(resolveJailed("~", "/root"), homedir());
});

// ── initLocalFolderSession ─────────────────────────────────────────────

test("initLocalFolderSession creates a session branch + scaffolds, and resumes", () => {
	const dir = mkdtempSync(join(tmpdir(), "gitagent-sess-"));
	try {
		const s = initLocalFolderSession(dir);

		// Branch shape + HEAD.
		assert.match(s.branch, /^gitagent\/session-[0-9a-f]{8}$/);
		assert.equal(s.sessionId, s.branch.replace("gitagent/session-", ""));
		const head = execSync("git symbolic-ref --short HEAD", { cwd: dir, encoding: "utf-8" }).trim();
		assert.equal(head, s.branch);

		// Scaffolded files.
		assert.ok(existsSync(join(dir, "agent.yaml")), "agent.yaml scaffolded");
		assert.ok(existsSync(join(dir, "memory", "MEMORY.md")), "memory scaffolded");

		// commitChanges makes a real commit.
		s.commitChanges("test commit");
		const count = execSync("git rev-list --count HEAD", { cwd: dir, encoding: "utf-8" }).trim();
		assert.equal(count, "1");

		// push() is a no-op (no remote) and must not throw.
		assert.doesNotThrow(() => s.push());

		// Resume the same branch.
		execSync("git checkout -b other", { cwd: dir }); // move off the session branch
		const resumed = initLocalFolderSession(dir, { session: s.branch });
		assert.equal(resumed.branch, s.branch);
		const head2 = execSync("git symbolic-ref --short HEAD", { cwd: dir, encoding: "utf-8" }).trim();
		assert.equal(head2, s.branch);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});
