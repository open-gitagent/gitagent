import { execSync } from "child_process";
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

function git(args: string, cwd: string): string {
	return execSync(`git ${args}`, { cwd, stdio: "pipe", encoding: "utf-8" }).trim();
}

function getDefaultBranch(cwd: string): string {
	try {
		// e.g. "origin/main" → "main"
		const ref = git("symbolic-ref refs/remotes/origin/HEAD", cwd);
		return ref.replace("refs/remotes/origin/", "");
	} catch {
		// Fallback: try main, then master
		try {
			git("rev-parse --verify origin/main", cwd);
			return "main";
		} catch {
			return "master";
		}
	}
}

/** Scaffold agent.yaml + memory/MEMORY.md in a session dir if missing. */
function scaffoldAgentFiles(dir: string, name: string): void {
	const agentYamlPath = `${dir}/agent.yaml`;
	if (!existsSync(agentYamlPath)) {
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
}

// ── initLocalSession ──────────────────────────────────────────────────

export function initLocalSession(opts: LocalRepoOptions): LocalSession {
	const { url, token, session } = opts;
	const dir = resolve(opts.dir);
	const aUrl = authedUrl(url, token);

	// Clone or update
	if (!existsSync(dir)) {
		execSync(`git clone --depth 1 --no-single-branch ${aUrl} ${dir}`, { stdio: "pipe" });
	} else {
		git(`remote set-url origin ${aUrl}`, dir);
		git("fetch origin", dir);

		// Reset local default branch to latest remote
		const defaultBranch = getDefaultBranch(dir);
		git(`checkout ${defaultBranch}`, dir);
		git(`reset --hard origin/${defaultBranch}`, dir);
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
			git(`checkout ${branch}`, dir);
		} catch {
			git(`checkout -b ${branch} origin/${branch}`, dir);
		}
		// Pull latest for existing session branch
		try { git(`pull origin ${branch}`, dir); } catch { /* branch may not exist on remote yet */ }
	} else {
		// New session — branch off latest default branch
		sessionId = randomBytes(4).toString("hex"); // 8-char hex
		branch = `gitagent/session-${sessionId}`;
		git(`checkout -b ${branch}`, dir);
	}

	// Scaffold agent.yaml + memory if missing (on session branch only)
	const name = url.split("/").pop()?.replace(/\.git$/, "") || "agent";
	scaffoldAgentFiles(dir, name);

	// Build session object
	const localSession: LocalSession = {
		dir,
		branch,
		sessionId,

		commitChanges(msg?: string) {
			git("add -A", dir);
			try {
				git("diff --cached --quiet", dir);
				// Nothing staged — skip
			} catch {
				// There are staged changes
				const commitMsg = msg || `gitagent: auto-commit (${branch})`;
				git(`commit -m "${commitMsg}"`, dir);
			}
		},

		push() {
			git(`push origin ${branch}`, dir);
		},

		finalize() {
			localSession.commitChanges();
			localSession.push();
			// Strip PAT from remote URL
			git(`remote set-url origin ${cleanUrl(url)}`, dir);
		},
	};

	return localSession;
}

// ── initLocalFolderSession (local-only, no GitHub) ────────────────────

export interface LocalFolderOptions {
	/** Resume an existing session branch instead of creating a new one. */
	session?: string;
}

/**
 * Cowork-style local session: attach to any local folder (no clone, no remote,
 * no PAT). Inits a git repo if needed, creates/resumes a
 * `gitagent/session-<hash>` branch, scaffolds agent.yaml + memory/, and
 * auto-commits locally. `push()` is a no-op. Used by the desktop app.
 */
export function initLocalFolderSession(dir: string, opts: LocalFolderOptions = {}): LocalSession {
	const resolved = resolve(dir);
	if (!existsSync(resolved)) {
		mkdirSync(resolved, { recursive: true });
	}

	// Ensure it's a git repo.
	let isRepo = false;
	try {
		git("rev-parse --is-inside-work-tree", resolved);
		isRepo = true;
	} catch { /* not a repo yet */ }
	if (!isRepo) {
		git("init", resolved);
	}

	// Ensure a committer identity exists (fresh machines/CI may lack one).
	try {
		git("config user.email", resolved);
	} catch {
		git('config user.email "agent@gitagent.local"', resolved);
		git('config user.name "gitagent"', resolved);
	}

	// Determine branch.
	let branch: string;
	let sessionId: string;
	if (opts.session) {
		branch = opts.session;
		sessionId = branch.replace(/^gitagent\/session-/, "") || branch;
		try {
			git(`checkout ${branch}`, resolved);
		} catch {
			git(`checkout -b ${branch}`, resolved);
		}
	} else {
		sessionId = randomBytes(4).toString("hex"); // 8-char hex
		branch = `gitagent/session-${sessionId}`;
		git(`checkout -b ${branch}`, resolved);
	}

	const name = resolved.split("/").filter(Boolean).pop() || "agent";
	scaffoldAgentFiles(resolved, name);

	const localSession: LocalSession = {
		dir: resolved,
		branch,
		sessionId,

		commitChanges(msg?: string) {
			git("add -A", resolved);
			try {
				git("diff --cached --quiet", resolved);
				// Nothing staged — skip
			} catch {
				const commitMsg = (msg || `gitagent: auto-commit (${branch})`).replace(/"/g, '\\"');
				git(`commit -m "${commitMsg}"`, resolved);
			}
		},

		// Local-only session — no remote to push to.
		push() { /* no-op */ },

		finalize() {
			localSession.commitChanges();
		},
	};

	return localSession;
}
