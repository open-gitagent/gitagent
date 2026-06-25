import { execSync } from "child_process";

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
		return execSync("git remote get-url origin", { cwd: dir, stdio: "pipe" })
			.toString()
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

	const apiKey = process.env.E2B_API_KEY;
	if (!apiKey) {
		throw new Error(
			"Sandbox mode requires an E2B API key. Set E2B_API_KEY in your environment.\n" +
			"Get one at https://e2b.dev (Dashboard → API Keys).",
		);
	}

	// gitmachine splits VM creation (E2BMachine) from git lifecycle (GitMachine):
	// build the provider machine first, then hand it to GitMachine. Note the key
	// names differ between the two — E2BMachine uses `envs`, GitMachine uses `env`.
	const machine = new gitmachine.E2BMachine({
		apiKey,
		template: config.template,
		timeout: config.timeout,
		envs: config.envs,
	});

	const gitMachine = new gitmachine.GitMachine({
		machine,
		repository,
		token,
		session: config.session,
		autoCommit: config.autoCommit ?? true,
		env: config.envs,
	});

	// gitmachine clones into a fixed path inside the VM (/home/user/repo).
	const repoPath = "/home/user/repo";

	return {
		gitMachine,
		machine,
		repoPath,
	};
}
