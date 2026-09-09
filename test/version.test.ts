import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile, readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

let v: typeof import("../dist/version.js");

before(async () => {
	process.env.GIT_CONFIG_GLOBAL = "/dev/null";
	process.env.GIT_CONFIG_SYSTEM = "/dev/null";
	process.env.GIT_CONFIG_NOSYSTEM = "1";
	v = await import("../dist/version.js");
});

function raw(cwd: string, ...args: string[]): string {
	return execFileSync("git", args, { cwd, env: process.env, stdio: "pipe", encoding: "utf-8" }).trim();
}

const MANIFEST = [
	'spec_version: "0.1.0"',
	"name: demo",
	"version: 1.0.0",
	"description: demo agent",
	"model:",
	'  preferred: "anthropic:claude-sonnet-4-5"',
	"  fallback: []",
	"tools: []",
	"runtime:",
	"  max_turns: 5",
	"",
].join("\n");

/** A git repo containing an agent, with one commit. `sub` nests the agent in a subdirectory. */
async function makeAgentRepo(sub = ""): Promise<{ root: string; agentDir: string }> {
	const root = await mkdtemp(join(tmpdir(), "gitagent-ver-"));
	raw(root, "init", "-q", "-b", "main");
	raw(root, "config", "user.email", "t@example.com");
	raw(root, "config", "user.name", "Test");

	const agentDir = sub ? join(root, sub) : root;
	await mkdir(join(agentDir, "tools"), { recursive: true });
	await mkdir(join(agentDir, "memory"), { recursive: true });
	await mkdir(join(agentDir, "skills", "demo"), { recursive: true });
	await writeFile(join(agentDir, "agent.yaml"), MANIFEST, "utf-8");
	await writeFile(join(agentDir, "SOUL.md"), "I am demo.\n", "utf-8");
	await writeFile(join(agentDir, "RULES.md"), "Be nice.\n", "utf-8");
	await writeFile(join(agentDir, "tools", "a.yaml"), "name: a\n", "utf-8");
	await writeFile(join(agentDir, "memory", "MEMORY.md"), "mem v1\n", "utf-8");
	await writeFile(join(agentDir, "skills", "demo", "SKILL.md"), "# demo\n", "utf-8");

	raw(root, "add", "-A");
	raw(root, "commit", "-qm", "init");
	return { root, agentDir };
}

function expectCode(fn: () => unknown, code: string): void {
	assert.throws(fn, (err: any) => err?.code === code, `expected VersionError code ${code}`);
}

const cleanup = (dir: string) => rm(dir, { recursive: true, force: true });

describe("version names", () => {
	it("accepts ordinary names and rejects unsafe ones", () => {
		for (const ok of ["v1.0", "prod-2024_06", "1", "V2"]) {
			assert.doesNotThrow(() => v.validateVersionName(ok), `${ok} should be valid`);
		}
		for (const bad of ["", "-x", "a b", "a..b", "HEAD", "foo.lock", "a/b", "v1~", "v1^", "x".repeat(200)]) {
			expectCode(() => v.validateVersionName(bad), "INVALID_NAME");
		}
	});
});

describe("preconditions", () => {
	it("rejects a directory that is not a git repo", async () => {
		const dir = await mkdtemp(join(tmpdir(), "gitagent-norepo-"));
		expectCode(() => v.resolveContext(dir), "NOT_A_REPO");
		await cleanup(dir);
	});

	it("rejects a repo without agent.yaml", async () => {
		const dir = await mkdtemp(join(tmpdir(), "gitagent-noyaml-"));
		raw(dir, "init", "-q", "-b", "main");
		expectCode(() => v.resolveContext(dir), "NO_AGENT_YAML");
		await cleanup(dir);
	});

	it("rejects a repo with no commits", async () => {
		const dir = await mkdtemp(join(tmpdir(), "gitagent-nocommit-"));
		raw(dir, "init", "-q", "-b", "main");
		await writeFile(join(dir, "agent.yaml"), MANIFEST, "utf-8");
		expectCode(() => v.resolveContext(dir), "NO_COMMITS");
		await cleanup(dir);
	});

	it("reports unknown versions with the known list", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1.0" });
		assert.throws(
			() => v.planRollback(ctx, "v9.9"),
			(err: any) => err.code === "UNKNOWN_VERSION" && err.message.includes("v1.0"),
		);
		await cleanup(root);
	});
});

describe("saveVersion", () => {
	it("creates an annotated tag and lists it", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		const saved = v.saveVersion(ctx, { name: "v1.0", message: "baseline" });

		assert.equal(saved.name, "v1.0");
		assert.equal(raw(root, "cat-file", "-t", "refs/tags/agentcfg/v1.0"), "tag");

		const list = v.listVersions(ctx);
		assert.equal(list.length, 1);
		assert.equal(list[0].name, "v1.0");
		assert.equal(list[0].subject, "baseline");
		assert.equal(list[0].annotated, true);

		await cleanup(root);
	});

	it("defaults the name to the manifest version", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const saved = v.saveVersion(v.resolveContext(agentDir), {});
		assert.equal(saved.name, "v1.0.0");
		await cleanup(root);
	});

	it("refuses a duplicate name unless forced", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1.0" });
		expectCode(() => v.saveVersion(ctx, { name: "v1.0" }), "TAG_EXISTS");
		assert.doesNotThrow(() => v.saveVersion(ctx, { name: "v1.0", force: true }));
		await cleanup(root);
	});

	it("refuses to tag over uncommitted config", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		await writeFile(join(agentDir, "SOUL.md"), "edited\n", "utf-8");
		assert.throws(
			() => v.saveVersion(ctx, { name: "v1.0" }),
			(err: any) => err.code === "DIRTY_CONFIG" && err.paths.includes("SOUL.md"),
		);
		await cleanup(root);
	});

	it("refuses when the index already has staged changes", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		await writeFile(join(agentDir, "memory", "MEMORY.md"), "staged\n", "utf-8");
		raw(root, "add", "memory/MEMORY.md");
		expectCode(() => v.saveVersion(ctx, { name: "v1.0" }), "INDEX_DIRTY");
		await cleanup(root);
	});

	// The property the whole feature is built around.
	it("commits config with --commit but never stages memory/", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		await writeFile(join(agentDir, "SOUL.md"), "edited soul\n", "utf-8");
		await writeFile(join(agentDir, "memory", "MEMORY.md"), "dirty memory\n", "utf-8");

		v.saveVersion(ctx, { name: "v1.0", commit: true });

		assert.equal(raw(root, "show", "refs/tags/agentcfg/v1.0:SOUL.md"), "edited soul");
		// The memory edit was neither staged nor committed.
		assert.ok(raw(root, "status", "--porcelain", "--", "memory").includes("memory/MEMORY.md"));

		await cleanup(root);
	});
});

describe("planRollback", () => {
	it("classifies added, deleted and modified config, and never memory/ or skills/", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1" });

		await writeFile(join(agentDir, "SOUL.md"), "changed\n", "utf-8");
		await writeFile(join(agentDir, "tools", "new.yaml"), "name: new\n", "utf-8");
		await rm(join(agentDir, "RULES.md"));
		await writeFile(join(agentDir, "memory", "MEMORY.md"), "mem v2\n", "utf-8");
		await mkdir(join(agentDir, "skills", "learned"), { recursive: true });
		await writeFile(join(agentDir, "skills", "learned", "SKILL.md"), "# learned\n", "utf-8");
		raw(root, "add", "-A");
		raw(root, "commit", "-qm", "changes");

		const plan = v.planRollback(ctx, "v1");
		assert.deepEqual(plan.restore, ["RULES.md", "SOUL.md"]);
		assert.deepEqual(plan.remove, ["tools/new.yaml"]);
		for (const p of [...plan.restore, ...plan.remove]) {
			assert.ok(!p.startsWith("memory/"), `${p} must not be in the plan`);
			assert.ok(!p.startsWith("skills/"), `${p} must not be in the plan`);
		}

		await cleanup(root);
	});
});

describe("applyRollback", () => {
	it("restores config, deletes files added later, and leaves memory/ alone", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1" });

		await writeFile(join(agentDir, "SOUL.md"), "changed\n", "utf-8");
		await writeFile(join(agentDir, "tools", "added-later.yaml"), "name: later\n", "utf-8");
		await rm(join(agentDir, "RULES.md"));
		await writeFile(join(agentDir, "memory", "MEMORY.md"), "mem AFTER tag\n", "utf-8");
		raw(root, "add", "-A");
		raw(root, "commit", "-qm", "changes");
		const before = raw(root, "rev-parse", "HEAD");

		const result = v.applyRollback(ctx, v.planRollback(ctx, "v1"));
		assert.equal(result.changed, true);

		assert.equal(await readFile(join(agentDir, "SOUL.md"), "utf-8"), "I am demo.\n");
		assert.equal(await readFile(join(agentDir, "RULES.md"), "utf-8"), "Be nice.\n");
		// A plain `git checkout <tag> -- tools/` would leave this behind.
		assert.equal(existsSync(join(agentDir, "tools", "added-later.yaml")), false);
		assert.ok(!raw(root, "ls-files").includes("added-later.yaml"));
		// The memory formed after the tag survives.
		assert.equal(await readFile(join(agentDir, "memory", "MEMORY.md"), "utf-8"), "mem AFTER tag\n");

		// Forward-only: one new commit, nothing rewritten, still on main.
		assert.equal(raw(root, "rev-list", "--count", `${before}..HEAD`), "1");
		assert.doesNotThrow(() => raw(root, "merge-base", "--is-ancestor", before, "HEAD"));
		assert.equal(raw(root, "rev-parse", "--abbrev-ref", "HEAD"), "main");

		await cleanup(root);
	});

	it("is a no-op when config already matches, with no empty commit", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1" });
		const count = raw(root, "rev-list", "--count", "HEAD");

		const result = v.applyRollback(ctx, v.planRollback(ctx, "v1"));
		assert.equal(result.changed, false);
		assert.equal(raw(root, "rev-list", "--count", "HEAD"), count);

		await cleanup(root);
	});

	it("restores a directory that was replaced by a file of the same name", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1" });

		await rm(join(agentDir, "tools"), { recursive: true });
		await writeFile(join(agentDir, "tools"), "now a file\n", "utf-8");
		raw(root, "add", "-A");
		raw(root, "commit", "-qm", "type flip");

		v.applyRollback(ctx, v.planRollback(ctx, "v1"));
		assert.equal(await readFile(join(agentDir, "tools", "a.yaml"), "utf-8"), "name: a\n");

		await cleanup(root);
	});

	it("handles paths with spaces and quotes", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		const weird = 'we ird "quoted".yaml';
		await writeFile(join(agentDir, "tools", weird), "name: w\n", "utf-8");
		raw(root, "add", "-A");
		raw(root, "commit", "-qm", "weird path");
		v.saveVersion(ctx, { name: "v1" });

		await rm(join(agentDir, "tools", weird));
		raw(root, "add", "-A");
		raw(root, "commit", "-qm", "remove weird");

		const plan = v.planRollback(ctx, "v1");
		assert.deepEqual(plan.restore, [`tools/${weird}`]);
		v.applyRollback(ctx, plan);
		assert.equal(existsSync(join(agentDir, "tools", weird)), true);

		await cleanup(root);
	});

	it("refuses a rollback that would overwrite uncommitted config unless forced", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1" });
		await writeFile(join(agentDir, "SOUL.md"), "uncommitted\n", "utf-8");

		expectCode(() => v.assertRollbackReady(ctx, false), "DIRTY_CONFIG");
		assert.doesNotThrow(() => v.assertRollbackReady(ctx, true));

		await cleanup(root);
	});

	it("refuses to roll back when the index already has staged changes", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1" });

		// Staged memory/ would otherwise be swept into the rollback commit.
		await writeFile(join(agentDir, "memory", "MEMORY.md"), "staged\n", "utf-8");
		raw(root, "add", "memory/MEMORY.md");

		expectCode(() => v.assertRollbackReady(ctx, false), "INDEX_DIRTY");
		// --force covers dirty config and detached HEAD, but never a dirty index.
		expectCode(() => v.assertRollbackReady(ctx, true), "INDEX_DIRTY");

		await cleanup(root);
	});

	it("refuses to roll back on a detached HEAD unless forced", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1" });
		raw(root, "checkout", "-q", "--detach");

		expectCode(() => v.assertRollbackReady(ctx, false), "DETACHED_HEAD");

		await cleanup(root);
	});
});

describe("diff scope", () => {
	it("never reports memory/ changes between two versions", async () => {
		const { root, agentDir } = await makeAgentRepo();
		const ctx = v.resolveContext(agentDir);
		v.saveVersion(ctx, { name: "v1" });

		await writeFile(join(agentDir, "SOUL.md"), "changed\n", "utf-8");
		await writeFile(join(agentDir, "memory", "MEMORY.md"), "mem v2\n", "utf-8");
		raw(root, "add", "-A");
		raw(root, "commit", "-qm", "changes");
		v.saveVersion(ctx, { name: "v2" });

		const args = v.buildDiffArgs(ctx, "v1", "v2", ["--name-only"]);
		const out = raw(root, ...args);
		assert.ok(out.includes("SOUL.md"));
		assert.ok(!out.includes("memory/"));
		assert.equal(v.hasConfigDiff(ctx, "v1", "v2"), true);

		await cleanup(root);
	});
});

describe("agent in a repo subdirectory", () => {
	it("scopes save and rollback to the agent's own paths", async () => {
		const { root, agentDir } = await makeAgentRepo("bot");
		await mkdir(join(root, "other"), { recursive: true });
		await writeFile(join(root, "other", "x.txt"), "untouched\n", "utf-8");
		raw(root, "add", "-A");
		raw(root, "commit", "-qm", "sibling project");

		const ctx = v.resolveContext(agentDir);
		assert.equal(ctx.prefix, "bot/");
		v.saveVersion(ctx, { name: "v1" });

		await writeFile(join(agentDir, "SOUL.md"), "changed\n", "utf-8");
		await writeFile(join(root, "other", "x.txt"), "also changed\n", "utf-8");
		raw(root, "add", "-A");
		raw(root, "commit", "-qm", "edits");

		const plan = v.planRollback(ctx, "v1");
		assert.deepEqual(plan.restore, ["bot/SOUL.md"]);
		v.applyRollback(ctx, plan);

		assert.equal(await readFile(join(agentDir, "SOUL.md"), "utf-8"), "I am demo.\n");
		assert.equal(await readFile(join(root, "other", "x.txt"), "utf-8"), "also changed\n");

		await cleanup(root);
	});
});
