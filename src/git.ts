import { execFileSync, spawnSync } from "child_process";

// A real config-tree diff blows past Node's 1 MiB default and throws ENOBUFS.
const MAX_BUFFER = 64 * 1024 * 1024;

export class GitError extends Error {
	constructor(
		message: string,
		readonly args: readonly string[],
		readonly status: number,
		readonly stderr: string,
	) {
		super(message);
		this.name = "GitError";
	}
}

function gitEnv(): NodeJS.ProcessEnv {
	return { ...process.env, GIT_TERMINAL_PROMPT: "0", GIT_OPTIONAL_LOCKS: "0" };
}

/**
 * Every git call in this module goes through here: an argv array, never an
 * interpolated string, so no caller can produce a shell-injection or quoting bug.
 * `--no-pager` keeps `diff` from launching `less` and appearing to hang; stdin is
 * closed so git can never block on a credential or editor prompt.
 */
function run(cwd: string, args: string[]): string {
	try {
		return execFileSync("git", ["--no-pager", ...args], {
			cwd,
			env: gitEnv(),
			stdio: ["ignore", "pipe", "pipe"],
			encoding: "utf-8",
			maxBuffer: MAX_BUFFER,
		});
	} catch (err: any) {
		const status = typeof err?.status === "number" ? err.status : -1;
		const stderr = typeof err?.stderr === "string" ? err.stderr.trim() : "";
		throw new GitError(stderr || err?.message || `git ${args.join(" ")} failed`, args, status, stderr);
	}
}

/** Trimmed stdout. Throws GitError on non-zero exit. */
export function git(cwd: string, args: string[]): string {
	return run(cwd, args).trim();
}

/** Raw stdout, NOT trimmed — required for `-z` NUL-separated plumbing output. */
export function gitRaw(cwd: string, args: string[]): string {
	return run(cwd, args);
}

/** Trimmed stdout, or null on any failure. Never throws. */
export function gitTry(cwd: string, args: string[]): string | null {
	try {
		return run(cwd, args).trim();
	} catch {
		return null;
	}
}

/** True iff git exited 0. For predicate commands like `diff --quiet`. */
export function gitOk(cwd: string, args: string[]): boolean {
	try {
		run(cwd, args);
		return true;
	} catch {
		return false;
	}
}

/** Trimmed stdout split into non-empty lines. */
export function gitLines(cwd: string, args: string[]): string[] {
	return run(cwd, args)
		.split("\n")
		.map((l) => l.trim())
		.filter(Boolean);
}

/**
 * Identity is injected per-invocation, never written to the user's config, and only
 * when the repo has none — `ensureRepo` leaves repos without one and `commit`/`tag -a`
 * hard-fail in that state. Signing is disabled unconditionally: a user with global
 * `commit.gpgsign=true` would otherwise hit a pinentry prompt with stdin closed.
 */
function identityArgs(cwd: string): string[] {
	const args = ["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"];
	if (!gitTry(cwd, ["config", "--get", "user.email"])) {
		args.push("-c", "user.name=GitAgent", "-c", "user.email=gitagent@localhost");
	}
	return args;
}

/** Mutating operations (commit, tag, rm, checkout). Throws GitError. */
export function gitWrite(cwd: string, args: string[]): string {
	return run(cwd, [...identityArgs(cwd), ...args]).trim();
}

/** Streams git's own stdout/stderr through, preserving colour. Returns the exit status. */
export function gitStream(cwd: string, args: string[]): number {
	const result = spawnSync("git", ["--no-pager", ...args], {
		cwd,
		env: gitEnv(),
		stdio: ["ignore", "inherit", "inherit"],
	});
	return result.status ?? -1;
}

export function isGitRepo(cwd: string): boolean {
	return gitOk(cwd, ["rev-parse", "--is-inside-work-tree"]);
}

export function isBareRepo(cwd: string): boolean {
	return gitTry(cwd, ["rev-parse", "--is-bare-repository"]) === "true";
}

export function isShallowRepo(cwd: string): boolean {
	return gitTry(cwd, ["rev-parse", "--is-shallow-repository"]) === "true";
}

export function hasCommits(cwd: string): boolean {
	return gitOk(cwd, ["rev-parse", "--verify", "-q", "HEAD"]);
}

export function repoRoot(cwd: string): string {
	return git(cwd, ["rev-parse", "--show-toplevel"]);
}

/** "" at the repo root, "bot/" when cwd is a subdirectory. */
export function repoPrefix(cwd: string): string {
	return git(cwd, ["rev-parse", "--show-prefix"]);
}

export function refExists(cwd: string, ref: string): boolean {
	return gitOk(cwd, ["rev-parse", "--verify", "-q", `${ref}^{}`]);
}
