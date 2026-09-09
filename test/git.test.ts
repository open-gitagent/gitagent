import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

let g: typeof import("../dist/git.js");

before(async () => {
	// Hermetic git: ignore the developer's ~/.gitconfig so CI and laptops agree, and so
	// the "repo has no identity" path is reachable on a machine that has a global one.
	process.env.GIT_CONFIG_GLOBAL = "/dev/null";
	process.env.GIT_CONFIG_SYSTEM = "/dev/null";
	process.env.GIT_CONFIG_NOSYSTEM = "1";
	g = await import("../dist/git.js");
});

// Raw git, deliberately not going through the module under test.
function raw(cwd: string, ...args: string[]): string {
	return execFileSync("git", args, { cwd, env: process.env, stdio: "pipe", encoding: "utf-8" }).trim();
}

async function makeRepo(identity = true): Promise<string> {
	const dir = await mkdtemp(join(tmpdir(), "gitagent-git-"));
	raw(dir, "init", "-q", "-b", "main");
	if (identity) {
		raw(dir, "config", "user.email", "t@example.com");
		raw(dir, "config", "user.name", "Test");
	}
	return dir;
}

describe("git helpers", () => {
	it("detects repos, bareness and commits", async () => {
		const plain = await mkdtemp(join(tmpdir(), "gitagent-plain-"));
		assert.equal(g.isGitRepo(plain), false);

		const dir = await makeRepo();
		assert.equal(g.isGitRepo(dir), true);
		assert.equal(g.isBareRepo(dir), false);
		assert.equal(g.hasCommits(dir), false);

		g.gitWrite(dir, ["commit", "--allow-empty", "-m", "first"]);
		assert.equal(g.hasCommits(dir), true);

		await rm(plain, { recursive: true, force: true });
		await rm(dir, { recursive: true, force: true });
	});

	it("returns trimmed stdout", async () => {
		const dir = await makeRepo();
		g.gitWrite(dir, ["commit", "--allow-empty", "-m", "first"]);
		assert.equal(g.git(dir, ["rev-parse", "--abbrev-ref", "HEAD"]), "main");
		await rm(dir, { recursive: true, force: true });
	});

	it("reports failure as GitError, null and false depending on the call", async () => {
		const dir = await makeRepo();
		g.gitWrite(dir, ["commit", "--allow-empty", "-m", "first"]);

		assert.throws(
			() => g.git(dir, ["rev-parse", "--verify", "refs/tags/nope"]),
			(err: any) => err.name === "GitError" && err.status !== 0 && typeof err.stderr === "string",
		);
		assert.equal(g.gitTry(dir, ["rev-parse", "--verify", "refs/tags/nope"]), null);
		assert.equal(g.gitOk(dir, ["rev-parse", "--verify", "refs/tags/nope"]), false);

		await rm(dir, { recursive: true, force: true });
	});

	// The reason this module exists: src/tools/memory.ts and src/session.ts build git
	// commands by string interpolation, so a quote in a commit message escapes the command.
	it("passes shell metacharacters through verbatim", async () => {
		const dir = await makeRepo();
		const evil = 'evil"; touch pwned; echo "';
		g.gitWrite(dir, ["commit", "--allow-empty", "-m", evil]);

		assert.equal(raw(dir, "log", "-1", "--format=%s"), evil);
		assert.equal(existsSync(join(dir, "pwned")), false);
		assert.equal(existsSync(join(process.cwd(), "pwned")), false);

		await rm(dir, { recursive: true, force: true });
	});

	it("handles paths containing quotes and command substitution", async () => {
		const dir = await makeRepo();
		const weird = 'we"ird $(touch owned).txt';
		await writeFile(join(dir, weird), "x", "utf-8");
		g.gitWrite(dir, ["add", "--", weird]);
		g.gitWrite(dir, ["commit", "-m", "add weird"]);

		// -z output is unquoted; git C-quotes such paths in human-readable output.
		assert.deepEqual(g.gitRaw(dir, ["ls-files", "-z"]).split("\0").filter(Boolean), [weird]);
		assert.equal(existsSync(join(dir, "owned")), false);

		await rm(dir, { recursive: true, force: true });
	});

	it("injects an identity only when the repo has none", async () => {
		const anon = await makeRepo(false);
		g.gitWrite(anon, ["commit", "--allow-empty", "-m", "x"]);
		assert.equal(raw(anon, "log", "-1", "--format=%ae"), "gitagent@localhost");

		const owned = await makeRepo(true);
		g.gitWrite(owned, ["commit", "--allow-empty", "-m", "x"]);
		assert.equal(raw(owned, "log", "-1", "--format=%ae"), "t@example.com");

		await rm(anon, { recursive: true, force: true });
		await rm(owned, { recursive: true, force: true });
	});

	it("gitRaw preserves NUL separators that gitLines would destroy", async () => {
		const dir = await makeRepo();
		await writeFile(join(dir, "a.txt"), "a", "utf-8");
		await writeFile(join(dir, "b.txt"), "b", "utf-8");
		g.gitWrite(dir, ["add", "-A"]);
		g.gitWrite(dir, ["commit", "-m", "two files"]);

		const out = g.gitRaw(dir, ["ls-tree", "-r", "--name-only", "-z", "HEAD"]);
		assert.ok(out.includes("\0"));
		assert.deepEqual(out.split("\0").filter(Boolean), ["a.txt", "b.txt"]);

		await rm(dir, { recursive: true, force: true });
	});

	it("reports the repo root and the subdirectory prefix", async () => {
		const dir = await makeRepo();
		g.gitWrite(dir, ["commit", "--allow-empty", "-m", "first"]);
		const sub = join(dir, "bot");
		await mkdir(sub, { recursive: true });

		assert.equal(g.repoPrefix(dir), "");
		assert.equal(g.repoPrefix(sub), "bot/");
		assert.equal(g.repoRoot(sub), g.repoRoot(dir));

		await rm(dir, { recursive: true, force: true });
	});

	it("resolves tag refs through refExists", async () => {
		const dir = await makeRepo();
		g.gitWrite(dir, ["commit", "--allow-empty", "-m", "first"]);
		g.gitWrite(dir, ["tag", "-a", "agentcfg/v1", "-m", "msg"]);

		assert.equal(g.refExists(dir, "refs/tags/agentcfg/v1"), true);
		assert.equal(g.refExists(dir, "refs/tags/agentcfg/v2"), false);

		await rm(dir, { recursive: true, force: true });
	});
});
