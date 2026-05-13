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

function authedUrl(url: string, token: string): string {
	// https://github.com/org/repo → https://<token>@github.com/org/repo
	return url.replace(/^https:\/\//, `https://${token}@`);
}

function cleanUrl(url: string): string {
	return url.replace(/^https:\/\/[^@]+@/, "https://");
}

/**
 * Safe git execution using argument arrays to prevent command injection.
 * Inputs are passed as separate arguments and never interpreted by a shell.
 */
function git(args: string[], cwd: string): string {
	return execFileSync("git", args, { cwd, stdio: "pipe", encoding: "utf-8" }).trim();
}

function getDefaultBranch(cwd: string): string {
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
	const { url, token, session } = opts;
	const dir = resolve(opts.dir);
	const aUrl = authedUrl(url, token);

	// Clone or update
	if (!existsSync(dir)) {
		execFileSync("git", ["clone", "--depth", "1", "--no-single-branch", aUrl, dir], { stdio: "pipe" });
	} else {
		git(["remote", "set-url", "origin", aUrl], dir);
		git(["fetch", "origin"], dir);

		// Reset local default branch to latest remote
		const defaultBranch = getDefaultBranch(dir);
		git(["checkout", defaultBranch], dir);
		git(["reset", "--hard", `origin/${defaultBranch}`], dir);
	}

	// Determine branch
	let branch: string;
	let sessionId: string;

	if (session) {
		// Resume existing session
		branch = session;
		sessionId = branch.replace(/^gitclaw\/session-/, "") || branch;

		// Try local checkout first, fall back to remote tracking
		try {
			git(["checkout", branch], dir);
		} catch {
			git(["checkout", "-b", branch, `origin/${branch}`], dir);
		}
		// Pull latest for existing session branch
		try { git(["pull", "origin", branch], dir); } catch { /* branch may not exist on remote yet */ }
	} else {
		// New session — branch off latest default branch
		sessionId = randomBytes(4).toString("hex"); // 8-char hex
		branch = `gitclaw/session-${sessionId}`;
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
			`description: Gitclaw agent for ${name}`,
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
				const commitMsg = msg || `gitclaw: auto-commit (${branch})`;
				git(["commit", "-m", commitMsg], dir);
			}
		},

		push() {
			git(["push", "origin", branch], dir);
		},

		finalize() {
			localSession.commitChanges();
			localSession.push();
			// Strip PAT from remote URL
			git(["remote", "set-url", "origin", cleanUrl(url)], dir);
		},
	};

	return localSession;
}
