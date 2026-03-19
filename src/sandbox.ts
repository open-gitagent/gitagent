import { execFileSync } from "child_process";

// ── Types ───────────────────────────────────────────────────────────────

export interface SandboxConfig {
	provider: "e2b";
	template?: string;
	timeout?: number;
	repository?: string;
	token?: string;
	session?: string;
	autoCommit?: boolean;
	envs?: Record<string, string>;
}

/**
 * Wraps the gitmachine GitMachine + Machine instances.
 * Types are `any` because gitmachine is an optional peer dependency
 * loaded via dynamic import — we don't have compile-time types.
 */
export interface SandboxContext {
	/** GitMachine instance (gitmachine) — provides run(), commit(), start(), stop() */
	gitMachine: any;
	/** Underlying Machine instance — provides readFile(), writeFile() */
	machine: any;
	/** Absolute path to the repo root inside the sandbox (e.g. /home/user/repo) */
	repoPath: string;
}

// ── Factory ─────────────────────────────────────────────────────────────

function detectRepoUrl(dir: string): string | null {
	try {
		return execFileSync("git", ["remote", "get-url", "origin"], { cwd: dir, stdio: "pipe", encoding: "utf-8" })
			.trim();
	} catch {
		return null;
	}
}

/**
 * Create a SandboxContext by dynamically importing gitmachine.
 * Throws a clear error if gitmachine is not installed.
 */
export async function createSandboxContext(
	config: SandboxConfig,
	dir: string,
): Promise<SandboxContext> {
	let gitmachine: any;
	try {
		// @ts-ignore — gitmachine is an optional peer dependency
		gitmachine = await import("gitmachine");
	} catch {
		throw new Error(
			"Sandbox mode requires the 'gitmachine' package.\n" +
			"Install it with: npm install gitmachine",
		);
	}

	const token = config.token
		|| process.env.GITHUB_TOKEN
		|| process.env.GIT_TOKEN;

	const repository = config.repository || detectRepoUrl(dir);
	if (!repository) {
		throw new Error(
			"Sandbox mode requires a repository URL. Provide it via --sandbox config, " +
			"or ensure the working directory has a git remote named 'origin'.",
		);
	}

	const gitMachine = new gitmachine.GitMachine({
		provider: config.provider,
		template: config.template,
		timeout: config.timeout,
		repository,
		token,
		session: config.session,
		autoCommit: config.autoCommit ?? true,
		envs: config.envs,
	});

	// The repo path inside the sandbox is determined by gitmachine after start().
	// Convention: /home/user/<repo-name>
	const repoName = repository.split("/").pop()?.replace(/\.git$/, "") || "repo";
	const repoPath = `/home/user/${repoName}`;

	return {
		gitMachine,
		machine: gitMachine.machine,
		repoPath,
	};
}
