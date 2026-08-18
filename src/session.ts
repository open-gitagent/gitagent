import { execSync, execFileSync } from "child_process";
import { existsSync, mkdirSync, writeFileSync, realpathSync } from "fs";
import { resolve } from "path";
import { randomBytes } from "crypto";

// ── Types ─────────────────────────────────────────────────────────────

export interface LocalRepoOptions {
	url: string;
	token: string;
	dir: string;
	session?: string;
	readOnly?: boolean;
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
	return url.replace(/^https:\/\/[^@]+@/, "https://").replace(/\.git$/, "");
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

// ── initLocalSession ──────────────────────────────────────────────────

export function initLocalSession(opts: LocalRepoOptions): LocalSession {
	const { url, token, session } = opts;
	if (!opts.dir) {
		throw new Error("repo.dir is required — refusing to guess a working directory for repo mode");
	}
	const dir = resolve(opts.dir);
	const aUrl = authedUrl(url, token);

	// Clone or update
	if (!existsSync(dir)) {
		execSync(`git clone --depth 1 --no-single-branch ${aUrl} ${dir}`, { stdio: "pipe" });
	} else {
		// Refuse to operate unless `dir` is a git repo root in its own right.
		// If `dir` has no local .git, git commands here would silently resolve
		// to whatever ancestor repo does have one — e.g. a monorepo root.
		let topLevel: string;
		try {
			topLevel = execSync("git rev-parse --show-toplevel", { cwd: dir, stdio: "pipe", encoding: "utf-8" }).trim();
		} catch {
			throw new Error(`${dir} exists but is not a git repository — refusing to operate on it`);
		}
		if (realpathSync(topLevel) !== realpathSync(dir)) {
			throw new Error(
				`${dir} has no .git of its own — git commands here would escape to the ancestor repo at ${topLevel}. Refusing to run destructive commands.`,
			);
		}

		// Refuse to reset a repo that's already tracking a different remote —
		// it isn't the clone this call expects, even if the folder exists.
		let existingOrigin = "";
		try {
			existingOrigin = execSync("git remote get-url origin", { cwd: dir, stdio: "pipe", encoding: "utf-8" }).trim();
		} catch {
			// no origin configured yet — fine, we're about to set one
		}
		if (existingOrigin && cleanUrl(existingOrigin) !== cleanUrl(url)) {
			throw new Error(
				`${dir} already tracks a different remote (${cleanUrl(existingOrigin)}), not ${url}. Refusing to overwrite it.`,
			);
		}

		// `set-url` only updates an existing remote — it errors if `origin`
		// isn't configured yet, which happens if `dir` is a repo the caller
		// created themselves rather than one gitagent cloned previously.
		git(`remote ${existingOrigin ? "set-url" : "add"} origin ${aUrl}`, dir);
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
			git("add -A", dir);
			try {
				git("diff --cached --quiet", dir);
				// Nothing staged — skip
			} catch {
				// There are staged changes. Use execFileSync with an argv array
				// (not the shell-interpolated `git()` helper) since commitMsg may
				// be an arbitrary caller-supplied string containing quotes, `$()`,
				// backticks, etc.
				const commitMsg = msg || `gitagent: auto-commit (${branch})`;
				execFileSync("git", ["commit", "-m", commitMsg], { cwd: dir, stdio: "pipe" });
			}
		},

		push() {
			git(`push origin ${branch}`, dir);
		},

		finalize() {
			if (!opts.readOnly) {
				localSession.commitChanges();
				localSession.push();
			}
			// Strip PAT from remote URL regardless of readOnly
			git(`remote set-url origin ${cleanUrl(url)}`, dir);
		},
	};

	return localSession;
}
