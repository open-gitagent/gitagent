import { execFileSync } from "child_process";
import { existsSync, mkdirSync, writeFileSync } from "fs";
import { resolve } from "path";
import { randomBytes } from "crypto";

// ── Types ─────────────────────────────────────────────────────────────

export interface LocalRepoOptions {
	url: string;
	token: string;
	dir: string;
	session?: string;
}

export interface LocalSession {
	dir: string;
	branch: string;
	sessionId: string;
	commitChanges(msg?: string): void;
	push(): void;
	finalize(): void;
}

// ── Helpers ───────────────────────────────────────────────────────────

function cleanUrl(url: string): string {
	return url.replace(/^https:\/\/[^@/]+@/, "https://");
}

/**
 * Environment that injects the token as an HTTP auth header via git's
 * GIT_CONFIG_* env mechanism, so it never lands in argv, the remote URL,
 * or .git/config.
 */
function authEnv(token: string): NodeJS.ProcessEnv {
	const basic = Buffer.from(`x-access-token:${token}`).toString("base64");
	return {
		...process.env,
		GIT_TERMINAL_PROMPT: "0",
		GIT_CONFIG_COUNT: "1",
		GIT_CONFIG_KEY_0: "http.extraHeader",
		GIT_CONFIG_VALUE_0: `Authorization: Basic ${basic}`,
	};
}

/** Remove any occurrence of the token (raw or base64-encoded) from text. */
export function redactToken(text: string, token: string): string {
	if (!token) return text;
	const secrets = [token, Buffer.from(`x-access-token:${token}`).toString("base64")];
	let out = text;
	for (const secret of secrets) out = out.split(secret).join("***");
	return out.replace(/^(https?:\/\/)[^@/\s]+@/gm, "$1***@");
}

function makeGit(token: string) {
	const env = authEnv(token);
	return function git(args: string[], cwd?: string): string {
		try {
			return execFileSync("git", args, { cwd, env, stdio: "pipe", encoding: "utf-8" }).trim();
		} catch (err: any) {
			const stderr = err?.stderr ? String(err.stderr).trim() : "";
			const msg = `git ${args[0]} failed${stderr ? `: ${stderr}` : ""}`;
			const e = new Error(redactToken(msg, token)) as Error & { status?: number };
			e.status = err?.status;
			throw e;
		}
	};
}

function assertSafeBranch(branch: string): void {
	if (!branch || branch.startsWith("-")) {
		throw new Error(`Invalid session branch name: ${branch}`);
	}
	try {
		execFileSync("git", ["check-ref-format", "--branch", branch], { stdio: "pipe" });
	} catch {
		throw new Error(`Invalid session branch name: ${branch}`);
	}
}

function getDefaultBranch(git: ReturnType<typeof makeGit>, cwd: string): string {
	try {
		// e.g. "origin/main" → "main"
		const ref = git(["symbolic-ref", "refs/remotes/origin/HEAD"], cwd);
		return ref.replace("refs/remotes/origin/", "");
	} catch {
		// Fallback: try main, then master
		try {
			git(["rev-parse", "--verify", "origin/main"], cwd);
			return "main";
		} catch {
			return "master";
		}
	}
}

// ── initLocalSession ──────────────────────────────────────────────────

export function initLocalSession(opts: LocalRepoOptions): LocalSession {
	const { token, session } = opts;
	const url = cleanUrl(opts.url);
	const dir = resolve(opts.dir);
	const git = makeGit(token);

	if (session) assertSafeBranch(session);

	// Clone or update
	if (!existsSync(dir)) {
		git(["clone", "--depth", "1", "--no-single-branch", "--", url, dir]);
	} else {
		// Ensure no credentials remain in the remote URL (older versions stored them there)
		git(["remote", "set-url", "origin", url], dir);
		git(["fetch", "origin"], dir);

		// Reset local default branch to latest remote
		const defaultBranch = getDefaultBranch(git, dir);
		git(["checkout", defaultBranch], dir);
		git(["reset", "--hard", `origin/${defaultBranch}`], dir);
	}

	// Determine branch
	let branch: string;
	let sessionId: string;

	if (session) {
		// Resume existing session
		branch = session;
		sessionId = branch.replace(/^gitagent\/session-/, "") || branch;

		// Try local checkout first, fall back to remote tracking
		try {
			git(["checkout", branch, "--"], dir);
		} catch {
			git(["checkout", "-b", branch, `origin/${branch}`, "--"], dir);
		}
		// Pull latest for existing session branch
		try { git(["pull", "origin", branch], dir); } catch { /* branch may not exist on remote yet */ }
	} else {
		// New session — branch off latest default branch
		sessionId = randomBytes(4).toString("hex"); // 8-char hex
		branch = `gitagent/session-${sessionId}`;
		git(["checkout", "-b", branch], dir);
	}

	// Scaffold agent.yaml + memory if missing (on session branch only)
	const agentYamlPath = `${dir}/agent.yaml`;
	if (!existsSync(agentYamlPath)) {
		const name = url.split("/").pop()?.replace(/\.git$/, "") || "agent";
		writeFileSync(agentYamlPath, [
			'spec_version: "0.1.0"',
			`name: ${name}`,
			"version: 0.1.0",
			`description: Gitagent agent for ${name}`,
			"model:",
			'  preferred: "openai:gpt-4o-mini"',
			"  fallback: []",
			"tools: [cli, read, write, memory]",
			"runtime:",
			"  max_turns: 50",
			"",
		].join("\n"), "utf-8");
	}

	const memoryFile = `${dir}/memory/MEMORY.md`;
	if (!existsSync(memoryFile)) {
		mkdirSync(`${dir}/memory`, { recursive: true });
		writeFileSync(memoryFile, "# Memory\n", "utf-8");
	}

	// Build session object
	const localSession: LocalSession = {
		dir,
		branch,
		sessionId,

		commitChanges(msg?: string) {
			git(["add", "-A"], dir);
			try {
				git(["diff", "--cached", "--quiet"], dir);
				// Nothing staged — skip
			} catch {
				// There are staged changes
				const commitMsg = msg || `gitagent: auto-commit (${branch})`;
				git(["commit", "-m", commitMsg], dir);
			}
		},

		push() {
			git(["push", "origin", branch], dir);
		},

		finalize() {
			try {
				localSession.commitChanges();
				localSession.push();
			} finally {
				// Credentials are never written to the remote URL, but make sure it is clean.
				try { git(["remote", "set-url", "origin", url], dir); } catch { /* best effort */ }
			}
		},
	};

	return localSession;
}
