// Regression tests for Issue #85 / PR #88:
//   Test 1 — Missing `dir` in repo mode must be rejected (both SDK query and initLocalSession)
//   Test 2 — Repo mode must not escape into a parent/ancestor git repository
//   Test 3 — Existing repo tracking a different origin remote must not be overwritten or reset
//   Test 4 — readOnly mode must skip commit and push while still scrubbing authentication tokens
//
// Follows repository convention: import compiled output from ../dist (run `npm run build` first).

import { describe, it, before, afterEach } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, mkdirSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

let initLocalSession: typeof import("../dist/session.js").initLocalSession;
let query: typeof import("../dist/exports.js").query;

// Disable all interactive credential helpers and prompts so tests are hermetic and fail fast
process.env.GIT_TERMINAL_PROMPT = "0";
process.env.GCM_INTERACTIVE = "never";

before(async () => {
	({ initLocalSession } = await import("../dist/session.js"));
	({ query } = await import("../dist/exports.js"));
});

describe("Issue #85 repo-mode safety properties", () => {
	const cleanupDirs: string[] = [];

	function createTempDir(prefix: string): string {
		const dir = mkdtempSync(join(tmpdir(), prefix));
		cleanupDirs.push(dir);
		return dir;
	}

	function initGitRepo(dir: string, bare = false): void {
		const args = bare ? ["init", "--bare"] : ["init"];
		execFileSync("git", args, { cwd: dir, stdio: "ignore" });
		if (!bare) {
			execFileSync("git", ["config", "user.name", "Test Runner"], { cwd: dir, stdio: "ignore" });
			execFileSync("git", ["config", "user.email", "test@example.com"], { cwd: dir, stdio: "ignore" });
			execFileSync("git", ["config", "credential.helper", ""], { cwd: dir, stdio: "ignore" });
			execFileSync("git", ["config", "core.askPass", ""], { cwd: dir, stdio: "ignore" });
		}
	}

	afterEach(() => {
		for (const d of cleanupDirs) {
			try {
				if (existsSync(d)) {
					rmSync(d, { recursive: true, force: true });
				}
			} catch {
				// Best-effort cleanup for Windows file locks
			}
		}
		cleanupDirs.length = 0;
	});

	// ── TEST 1: Missing dir rejection ──────────────────────────────────────

	describe("Safety Property 1: missing dir rejection", () => {
		it("initLocalSession rejects when dir is omitted or empty", () => {
			const isolatedCwd = createTempDir("gitagent-isolated-cwd-");
			const origCwd = process.cwd();
			try {
				process.chdir(isolatedCwd);
				assert.throws(
					() => {
						initLocalSession({
							url: "https://github.com/org/dummy-repo.git",
							token: "fake-token",
						} as any);
					},
					/repo\.dir is required|refusing to guess a working directory/i,
					"initLocalSession must throw when dir is not provided",
				);
			} finally {
				process.chdir(origCwd);
			}
		});

		it("query() rejects when repo mode has no explicit top-level or repo.dir", async () => {
			const isolatedCwd = createTempDir("gitagent-isolated-cwd-");
			const origCwd = process.cwd();
			try {
				process.chdir(isolatedCwd);
				const messages: any[] = [];
				for await (const msg of query({
					prompt: "verify repo safety",
					repo: {
						url: "https://github.com/org/dummy-repo.git",
						token: "fake-token",
					},
				} as any)) {
					messages.push(msg);
				}
				const errorMsg = messages.find((m: any) => m.type === "system" && m.subtype === "error");
				assert.ok(errorMsg, "query() must emit a system error message when dir is omitted in repo mode");
				assert.match(
					errorMsg.content,
					/repo mode requires an explicit `dir`|refusing to default to the current working directory/i,
				);
			} finally {
				process.chdir(origCwd);
			}
		});
	});

	// ── TEST 2: Ancestor repository protection ─────────────────────────────

	describe("Safety Property 2: ancestor repository protection", () => {
		it("refuses to operate when target directory is a child folder inside an ancestor git repo", () => {
			const parentRepo = createTempDir("gitagent-ancestor-parent-");
			initGitRepo(parentRepo);

			// Parent repository tracks its own legitimate origin
			execFileSync("git", ["remote", "add", "origin", "file:///parent-legit-remote.git"], {
				cwd: parentRepo,
				stdio: "ignore",
			});

			// Create a child directory that has no .git of its own
			const childSubdir = join(parentRepo, "nested-subfolder");
			mkdirSync(childSubdir, { recursive: true });

			assert.throws(
				() => {
					initLocalSession({
						url: "file:///dummy-victim-repo.git",
						token: "fake-token",
						dir: childSubdir,
					});
				},
				/has no \.git of its own — git commands here would escape to the ancestor repo/i,
				"Must detect that child folder is not its own git root and refuse to run destructive commands",
			);

			// Parent repo's remote origin must remain intact and not overwritten
			const parentOrigin = execFileSync("git", ["remote", "get-url", "origin"], {
				cwd: parentRepo,
				encoding: "utf-8",
			}).trim();
			assert.match(parentOrigin, /parent-legit-remote/, "Parent repository origin must not be touched");
		});
	});

	// ── TEST 3: Mismatched remote protection ───────────────────────────────

	describe("Safety Property 3: mismatched remote protection", () => {
		it("refuses to overwrite an existing repo tracking a different remote origin", () => {
			const targetRepo = createTempDir("gitagent-mismatch-repo-");
			initGitRepo(targetRepo);

			// Existing repo points to alpha-project
			execFileSync("git", ["remote", "add", "origin", "https://github.com/org/alpha-project.git"], {
				cwd: targetRepo,
				stdio: "ignore",
			});

			assert.throws(
				() => {
					initLocalSession({
						url: "https://github.com/org/beta-project.git",
						token: "fake-token",
						dir: targetRepo,
					});
				},
				/already tracks a different remote.*Refusing to overwrite/i,
				"Must detect existing remote origin mismatch and refuse to overwrite",
			);

			// Invariant: original remote URL must remain intact
			const currentOrigin = execFileSync("git", ["remote", "get-url", "origin"], {
				cwd: targetRepo,
				encoding: "utf-8",
			}).trim();
			assert.match(currentOrigin, /alpha-project/, "Existing remote origin must not be modified");
		});
	});

	// ── TEST 4: readOnly mode and token scrubbing ──────────────────────────

	describe("Safety Property 4: readOnly mode and token scrubbing", () => {
		it("readOnly: true avoids creating commits and pushing branches", () => {
			const bareRemote = createTempDir("gitagent-readonly-remote-");
			initGitRepo(bareRemote, true);

			const localRepo = createTempDir("gitagent-readonly-local-");
			initGitRepo(localRepo);

			// Commit an initial file so a default branch exists
			writeFileSync(join(localRepo, "initial.txt"), "committed content\n");
			execFileSync("git", ["add", "initial.txt"], { cwd: localRepo, stdio: "ignore" });
			execFileSync("git", ["commit", "-m", "initial commit"], { cwd: localRepo, stdio: "ignore" });
			execFileSync("git", ["remote", "add", "origin", bareRemote], { cwd: localRepo, stdio: "ignore" });
			execFileSync("git", ["push", "-u", "origin", "HEAD"], { cwd: localRepo, stdio: "ignore" });

			const initialSha = execFileSync("git", ["rev-parse", "HEAD"], {
				cwd: localRepo,
				encoding: "utf-8",
			}).trim();

			const session = initLocalSession({
				url: bareRemote,
				token: "fake-token",
				dir: localRepo,
				readOnly: true,
			} as any);

			// Stage an uncommitted change in the working tree
			writeFileSync(join(localRepo, "staged-change.txt"), "should not be committed\n");
			execFileSync("git", ["add", "staged-change.txt"], { cwd: localRepo, stdio: "ignore" });

			// Finalize should skip commitChanges() and push() under readOnly: true
			session.finalize();

			// 1. Invariant: no new commit created in local working tree
			const finalSha = execFileSync("git", ["rev-parse", "HEAD"], {
				cwd: localRepo,
				encoding: "utf-8",
			}).trim();
			assert.equal(finalSha, initialSha, "readOnly mode must not create new commits during finalize()");

			// 2. Invariant: staged file remains uncommitted in working tree
			const statusOutput = execFileSync("git", ["status", "--porcelain"], {
				cwd: localRepo,
				encoding: "utf-8",
			});
			assert.match(statusOutput, /A\s+staged-change\.txt/, "Staged changes must remain uncommitted");

			// 3. Invariant: no branch was pushed to bare remote
			const bareBranches = execFileSync("git", ["branch"], {
				cwd: bareRemote,
				encoding: "utf-8",
			});
			assert.equal(
				bareBranches.includes(session.branch),
				false,
				"readOnly mode must not push any session branches to remote",
			);
		});

		it("finalize() scrubs embedded credentials from origin remote URL", () => {
			const bareRemote = createTempDir("gitagent-scrub-remote-");
			initGitRepo(bareRemote, true);

			const localRepo = createTempDir("gitagent-scrub-local-");
			initGitRepo(localRepo);

			// Commit an initial file and push to bare remote
			writeFileSync(join(localRepo, "initial.txt"), "content\n");
			execFileSync("git", ["add", "initial.txt"], { cwd: localRepo, stdio: "ignore" });
			execFileSync("git", ["commit", "-m", "initial commit"], { cwd: localRepo, stdio: "ignore" });
			execFileSync("git", ["remote", "add", "origin", bareRemote], { cwd: localRepo, stdio: "ignore" });
			execFileSync("git", ["push", "-u", "origin", "HEAD"], { cwd: localRepo, stdio: "ignore" });

			const secretToken = "ghp_superSecretToken98765";
			const repoUrl = "https://github.com/org/scrub-project.git";
			const authed = `https://${secretToken}@github.com/org/scrub-project.git`;
			const bareUri = `file:///${bareRemote.replace(/\\/g, "/")}`;

			// Set origin to repoUrl (simulates repository tracking this project)
			execFileSync("git", ["remote", "set-url", "origin", repoUrl], { cwd: localRepo });

			// Redirect git fetch/push traffic for the remote to bareRemote offline
			execFileSync("git", ["config", `url.${bareUri}.insteadOf`, authed], { cwd: localRepo });

			const session = initLocalSession({
				url: repoUrl,
				token: secretToken,
				dir: localRepo,
				readOnly: true,
			} as any);

			// Origin remote was configured with embedded token during session
			session.finalize();

			// Invariant: after finalize, origin remote must have token scrubbed
			const sanitizedOrigin = execFileSync("git", ["config", "--get", "remote.origin.url"], {
				cwd: localRepo,
				encoding: "utf-8",
			}).trim();

			assert.equal(
				sanitizedOrigin.includes(secretToken),
				false,
				"Authentication token must be scrubbed from remote URL",
			);
			assert.match(
				sanitizedOrigin,
				/^https:\/\/github\.com\/org\/scrub-project(\.git)?$/,
				"Remote URL must be restored to canonical clean form",
			);
		});
	});
});
