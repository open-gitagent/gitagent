#!/usr/bin/env node

import { createInterface } from "readline";
import { Agent } from "@mariozechner/pi-agent-core";
import type { AgentEvent, AgentTool } from "@mariozechner/pi-agent-core";
import { loadAgent } from "./loader.js";
import { createBuiltinTools } from "./tools/index.js";
import { createSandboxContext } from "./sandbox.js";
import type { SandboxContext, SandboxConfig } from "./sandbox.js";
import { expandSkillCommand, refreshSkills } from "./skills.js";
import { loadHooksConfig, runHooks, wrapToolWithHooks } from "./hooks.js";
import type { HooksConfig } from "./hooks.js";
import { loadDeclarativeTools } from "./tool-loader.js";
import { toAgentTool } from "./tool-utils.js";
import { AuditLogger, isAuditEnabled } from "./audit.js";
import { formatComplianceWarnings } from "./compliance.js";
import { readFile, mkdir, writeFile, stat, access } from "fs/promises";
import { join, resolve } from "path";
import { execSync } from "child_process";
import { initLocalSession } from "./session.js";
import type { LocalSession } from "./session.js";
import { startVoiceServer } from "./voice/server.js";
import { handlePluginCommand } from "./plugin-cli.js";

// ANSI helpers
const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const bold = (s: string) => `\x1b[1m${s}\x1b[0m`;
const red = (s: string) => `\x1b[31m${s}\x1b[0m`;
const green = (s: string) => `\x1b[32m${s}\x1b[0m`;

interface ParsedArgs {
	model?: string;
	dir: string;
	prompt?: string;
	env?: string;
	sandbox?: boolean;
	sandboxRepo?: string;
	sandboxToken?: string;
	repo?: string;
	pat?: string;
	session?: string;
	voice?: string;
}

function parseArgs(argv: string[]): ParsedArgs {
	const args = argv.slice(2);
	let model: string | undefined;
	let dir = process.cwd();
	let prompt: string | undefined;
	let env: string | undefined;
	let sandbox = false;
	let sandboxRepo: string | undefined;
	let sandboxToken: string | undefined;
	let repo: string | undefined;
	let pat: string | undefined;
	let session: string | undefined;
	let voice: string | undefined;

	for (let i = 0; i < args.length; i++) {
		switch (args[i]) {
			case "--model":
			case "-m":
				model = args[++i];
				break;
			case "--dir":
			case "-d":
				dir = args[++i];
				break;
			case "--prompt":
			case "-p":
				prompt = args[++i];
				break;
			case "--env":
			case "-e":
				env = args[++i];
				break;
			case "--sandbox":
			case "-s":
				sandbox = true;
				break;
			case "--sandbox-repo":
				sandboxRepo = args[++i];
				break;
			case "--sandbox-token":
				sandboxToken = args[++i];
				break;
			case "--repo":
			case "-r":
				repo = args[++i];
				break;
			case "--pat":
				pat = args[++i];
				break;
			case "--session":
				session = args[++i];
				break;
			case "--voice":
			case "-v":
				// Accept optional backend name: --voice, --voice openai, --voice gemini
				if (args[i + 1] && !args[i + 1].startsWith("-")) {
					voice = args[++i];
				} else {
					voice = "openai";
				}
				break;
			default:
				if (!args[i].startsWith("-")) {
					prompt = args[i];
				}
				break;
		}
	}

	return { model, dir, prompt, env, sandbox, sandboxRepo, sandboxToken, repo, pat, session, voice };
}

function handleEvent(
	event: AgentEvent,
	hooksConfig: HooksConfig | null,
	agentDir: string,
	sessionId: string,
	auditLogger?: AuditLogger,
): void {
	switch (event.type) {
		case "message_update": {
			const e = event.assistantMessageEvent;
			if (e.type === "text_delta") {
				process.stdout.write(e.delta);
			}
			break;
		}
		case "message_end": {
			process.stdout.write("\n");
			// Fire post_response hooks (non-blocking)
			if (hooksConfig?.hooks.post_response) {
				runHooks(hooksConfig.hooks.post_response, agentDir, {
					event: "post_response",
					session_id: sessionId,
				}).catch(() => {});
			}
			auditLogger?.logResponse().catch(() => {});
			break;
		}
		case "tool_execution_start":
			process.stdout.write(dim(`\n▶ ${event.toolName}(${summarizeArgs(event.args)})\n`));
			auditLogger?.logToolUse(event.toolName, event.args || {}).catch(() => {});
			break;
		case "tool_execution_end": {
			if (event.isError) {
				process.stdout.write(red(`✗ ${event.toolName} failed\n`));
			} else {
				const result = event.result;
				const text = result?.content?.[0]?.text || "";
				const preview = text.length > 200 ? text.slice(0, 200) + "…" : text;
				if (preview) {
					process.stdout.write(dim(preview) + "\n");
				}
			}
			break;
		}
		case "agent_end":
			break;
	}
}

function summarizeArgs(args: any): string {
	if (!args) return "";
	const entries = Object.entries(args);
	if (entries.length === 0) return "";

	return entries
		.map(([k, v]) => {
			const str = typeof v === "string" ? v : JSON.stringify(v);
			const short = str.length > 60 ? str.slice(0, 60) + "…" : str;
			return `${k}: ${short}`;
		})
		.join(", ");
}

function askQuestion(question: string): Promise<string> {
	const rl = createInterface({ input: process.stdin, output: process.stdout });
	return new Promise((res) => {
		rl.question(question, (answer) => {
			rl.close();
			res(answer.trim());
		});
	});
}

function isGitRepo(dir: string): boolean {
	try {
		execSync("git rev-parse --is-inside-work-tree", { cwd: dir, stdio: "pipe" });
		return true;
	} catch {
		return false;
	}
}

async function fileExists(path: string): Promise<boolean> {
	try {
		await access(path);
		return true;
	} catch {
		return false;
	}
}

async function ensureRepo(dir: string, model?: string): Promise<string> {
	const absDir = resolve(dir);

	// Create directory if it doesn't exist
	if (!(await fileExists(absDir))) {
		console.log(dim(`Creating directory: ${absDir}`));
		await mkdir(absDir, { recursive: true });
	}

	// Git init if not a repo
	if (!isGitRepo(absDir)) {
		console.log(dim("Initializing git repository..."));
		execSync("git init", { cwd: absDir, stdio: "pipe" });

		// Create .gitignore
		const gitignorePath = join(absDir, ".gitignore");
		if (!(await fileExists(gitignorePath))) {
			await writeFile(gitignorePath, "node_modules/\ndist/\n.gitagent/\n", "utf-8");
		}

		// Initial commit so memory saves work
		execSync("git add -A && git commit -m 'Initial commit' --allow-empty", {
			cwd: absDir,
			stdio: "pipe",
		});
	}

	// Scaffold agent.yaml if missing
	const agentYamlPath = join(absDir, "agent.yaml");
	if (!(await fileExists(agentYamlPath))) {
		const defaultModel = model || "openai:gpt-4o-mini";
		const agentName = absDir.split("/").pop() || "my-agent";
		const yaml = [
			'spec_version: "0.1.0"',
			`name: ${agentName}`,
			"version: 0.1.0",
			`description: Gitclaw agent for ${agentName}`,
			"model:",
			`  preferred: "${defaultModel}"`,
			"  fallback: []",
			"tools: [cli, read, write, memory]",
			"runtime:",
			"  max_turns: 50",
			"",
		].join("\n");
		await writeFile(agentYamlPath, yaml, "utf-8");
		console.log(dim(`Created agent.yaml (model: ${defaultModel})`));
	}

	// Scaffold workspace directory
	const workspaceDir = join(absDir, "workspace");
	if (!(await fileExists(workspaceDir))) {
		await mkdir(workspaceDir, { recursive: true });
	}

	// Scaffold memory if missing
	const memoryDir = join(absDir, "memory");
	const memoryFile = join(memoryDir, "MEMORY.md");
	if (!(await fileExists(memoryFile))) {
		await mkdir(memoryDir, { recursive: true });
		await writeFile(memoryFile, "# Memory\n", "utf-8");
	}

	// Scaffold SOUL.md if missing
	const soulPath = join(absDir, "SOUL.md");
	if (!(await fileExists(soulPath))) {
		await writeFile(soulPath, [
			"# Identity",
			"",
			"You are a helpful AI agent. You live inside a git repository.",
			"You can run commands, read and write files, and remember things.",
			"Be concise and action-oriented.",
			"",
		].join("\n"), "utf-8");
	}

	// Stage new scaffolded files
	try {
		execSync("git add -A && git diff --cached --quiet || git commit -m 'Scaffold gitclaw agent'", {
			cwd: absDir,
			stdio: "pipe",
		});
	} catch {
		// ok if nothing to commit
	}

	return absDir;
}

async function main(): Promise<void> {
	// Handle plugin subcommand: gitclaw plugin <install|list|remove|...>
	if (process.argv[2] === "plugin") {
		const allArgs = process.argv.slice(3);
		let agentDir = process.cwd();
		const pluginArgs: string[] = [];
		for (let i = 0; i < allArgs.length; i++) {
			if ((allArgs[i] === "--dir" || allArgs[i] === "-d") && allArgs[i + 1]) {
				agentDir = allArgs[++i];
			} else {
				pluginArgs.push(allArgs[i]);
			}
		}
		await handlePluginCommand(resolve(agentDir), pluginArgs);
		return;
	}

	const { model, dir: rawDir, prompt, env, sandbox: useSandbox, sandboxRepo, sandboxToken, repo, pat, session: sessionBranch, voice } = parseArgs(process.argv);

	// If --repo is given, derive a default dir from the repo URL (skip interactive prompt)
	let dir = rawDir;
	let localSession: LocalSession | undefined;

	if (repo) {
		// Validate mutually exclusive flags
		if (useSandbox) {
			console.error(red("Error: --repo and --sandbox are mutually exclusive"));
			process.exit(1);
		}

		const token = pat || process.env.GITHUB_TOKEN || process.env.GIT_TOKEN;
		if (!token) {
			console.error(red("Error: --pat, GITHUB_TOKEN, or GIT_TOKEN is required with --repo"));
			process.exit(1);
		}

		// Default dir: /tmp/gitclaw/<repo-name> if no --dir given
		if (dir === process.cwd()) {
			const repoName = repo.split("/").pop()?.replace(/\.git$/, "") || "repo";
			dir = resolve(`/tmp/gitclaw/${repoName}`);
		}

		localSession = initLocalSession({
			url: repo,
			token,
			dir,
			session: sessionBranch,
		});
		dir = localSession.dir;
		console.log(dim(`Local session: ${localSession.branch} (${localSession.dir})`));
	} else if (dir === process.cwd() && !prompt) {
		// No --repo: interactive prompt for dir
		const answer = await askQuestion(green("? ") + bold("Repository path") + dim(" (. for current dir)") + green(": "));
		if (answer) {
			dir = resolve(answer === "." ? process.cwd() : answer);
		}
	}

	// Create sandbox context if --sandbox flag is set
	let sandboxCtx: SandboxContext | undefined;
	if (useSandbox) {
		const sandboxConfig: SandboxConfig = {
			provider: "e2b",
			repository: sandboxRepo,
			token: sandboxToken,
		};
		sandboxCtx = await createSandboxContext(sandboxConfig, resolve(dir));
		console.log(dim("Starting sandbox VM..."));
		await sandboxCtx.gitMachine.start();
		console.log(dim(`Sandbox ready (repo: ${sandboxCtx.repoPath})`));
	}

	// Ensure the target is a valid gitclaw repo (skip in sandbox/local-repo mode)
	if (localSession) {
		// Already cloned and scaffolded by initLocalSession
	} else if (!useSandbox) {
		dir = await ensureRepo(dir, model);
	} else {
		dir = resolve(dir);
	}

	// Voice mode
	if (voice) {
		let adapterBackend: "openai-realtime" | "gemini-live";
		let apiKey: string | undefined;

		if (voice === "gemini") {
			adapterBackend = "gemini-live";
			apiKey = process.env.GEMINI_API_KEY;
			if (!apiKey) {
				console.error(red("Error: GEMINI_API_KEY is required for --voice gemini"));
				process.exit(1);
			}
		} else {
			adapterBackend = "openai-realtime";
			apiKey = process.env.OPENAI_API_KEY;
			if (!apiKey) {
				console.error(red("Error: OPENAI_API_KEY is required for --voice mode"));
				process.exit(1);
			}
		}

		const cleanup = await startVoiceServer({
			adapter: adapterBackend,
			adapterConfig: { apiKey },
			agentDir: dir,
			model,
			env,
		});

		let stopping = false;
		process.on("SIGINT", () => {
			if (stopping) {
				// Second Ctrl+C — force exit immediately
				process.exit(1);
			}
			stopping = true;
			console.log("\nDisconnecting...");
			cleanup().finally(() => process.exit(0));
		});

		// Keep process alive
		return;
	}

	let loaded;
	try {
		loaded = await loadAgent(dir, model, env);
	} catch (err: any) {
		console.error(red(`Error: ${err.message}`));
		process.exit(1);
	}

	const { systemPrompt, manifest, skills, sessionId, agentDir, gitagentDir, complianceWarnings } = loaded;

	// Show compliance warnings
	if (complianceWarnings.length > 0) {
		const yellow = (s: string) => `\x1b[33m${s}\x1b[0m`;
		console.log(yellow("Compliance warnings:"));
		console.log(yellow(formatComplianceWarnings(complianceWarnings)));
	}

	// Initialize audit logger
	const auditEnabled = isAuditEnabled(manifest.compliance);
	const auditLogger = new AuditLogger(gitagentDir, sessionId, auditEnabled);
	if (auditEnabled) {
		await auditLogger.logSessionStart();
	}

	// Load hooks config (agent + plugin hooks merged)
	const { mergeHooksConfigs } = await import("./plugins.js");
	const agentHooksConfig = await loadHooksConfig(agentDir);
	const hooksConfig = mergeHooksConfigs(agentHooksConfig, loaded.plugins);

	// Run on_session_start hooks
	if (hooksConfig?.hooks.on_session_start) {
		try {
			const result = await runHooks(hooksConfig.hooks.on_session_start, agentDir, {
				event: "on_session_start",
				session_id: sessionId,
				agent: manifest.name,
			});
			if (result.action === "block") {
				console.error(red(`Session blocked by hook: ${result.reason || "no reason given"}`));
				process.exit(1);
			}
		} catch (err: any) {
			console.error(red(`Hook error: ${err.message}`));
		}
	}

	// Map provider to expected env var
	const apiKeyEnvVars: Record<string, string> = {
		anthropic: "ANTHROPIC_API_KEY",
		openai: "OPENAI_API_KEY",
		google: "GOOGLE_API_KEY",
		xai: "XAI_API_KEY",
		groq: "GROQ_API_KEY",
		mistral: "MISTRAL_API_KEY",
	};

	const provider = loaded.model.provider;
	const envVar = apiKeyEnvVars[provider];
	if (envVar && !process.env[envVar]) {
		console.error(red(`Error: ${envVar} environment variable is not set.`));
		console.error(dim(`Set it with: export ${envVar}=your-key-here`));
		process.exit(1);
	}

	// Collect plugin memory layers
	const pluginMemoryLayers = loaded.plugins.flatMap((p) => p.memoryLayers);

	// Build tools — built-in + declarative
	let tools: AgentTool<any>[] = createBuiltinTools({
		dir,
		timeout: manifest.runtime.timeout,
		sandbox: sandboxCtx,
		gitagentDir,
		pluginMemoryLayers: pluginMemoryLayers.length > 0 ? pluginMemoryLayers : undefined,
	});

	// Load declarative tools from tools/*.yaml (Phase 2.2)
	const declarativeTools = await loadDeclarativeTools(agentDir);
	tools = [...tools, ...declarativeTools];

	// Plugin tools (declarative + programmatic) — check for collisions with existing tools
	const existingToolNames = new Set(tools.map((t) => t.name));
	for (const plugin of loaded.plugins) {
		const pluginTools = [
			...plugin.tools,
			...plugin.programmaticTools.map(toAgentTool),
		];
		for (const t of pluginTools) {
			if (existingToolNames.has(t.name)) {
				console.warn(`[plugin:${plugin.manifest.id}] Tool "${t.name}" collides with existing tool — skipping`);
			} else {
				tools.push(t);
				existingToolNames.add(t.name);
			}
		}
	}

	// Wrap with hooks if configured
	if (hooksConfig) {
		tools = tools.map((t) => wrapToolWithHooks(t, hooksConfig, agentDir, sessionId));
	}

	// Build model options from manifest constraints
	const modelOptions: Record<string, any> = {};
	if (manifest.model.constraints) {
		const c = manifest.model.constraints;
		if (c.temperature !== undefined) modelOptions.temperature = c.temperature;
		if (c.max_tokens !== undefined) modelOptions.maxTokens = c.max_tokens;
		if (c.top_p !== undefined) modelOptions.topP = c.top_p;
		if (c.top_k !== undefined) modelOptions.topK = c.top_k;
		if (c.stop_sequences !== undefined) modelOptions.stopSequences = c.stop_sequences;
	}

	const agent = new Agent({
		initialState: {
			systemPrompt,
			model: loaded.model,
			tools,
			...modelOptions,
		},
	});

	agent.subscribe((event) => handleEvent(event, hooksConfig, agentDir, sessionId, auditLogger));

	console.log(bold(`${manifest.name} v${manifest.version}`));
	console.log(dim(`Model: ${loaded.model.provider}:${loaded.model.id}`));
	const allToolNames = tools.map((t) => t.name);
	console.log(dim(`Tools: ${allToolNames.join(", ")}`));
	if (skills.length > 0) {
		console.log(dim(`Skills: ${skills.map((s) => s.name).join(", ")}`));
	}
	if (loaded.workflows.length > 0) {
		console.log(dim(`Workflows: ${loaded.workflows.map((w) => w.name).join(", ")}`));
	}
	if (loaded.subAgents.length > 0) {
		console.log(dim(`Agents: ${loaded.subAgents.map((a) => a.name).join(", ")}`));
	}
	if (loaded.plugins.length > 0) {
		console.log(dim(`Plugins: ${loaded.plugins.map((p) => p.manifest.id).join(", ")}`));
	}
	console.log(dim('Type /skills to list skills, /plugins to list plugins, /memory to view memory, /quit to exit\n'));

	// Single-shot mode
	if (prompt) {
		try {
			await agent.prompt(prompt);
		} catch (err: any) {
			auditLogger?.logError(err.message).catch(() => {});
			// Fire on_error hooks
			if (hooksConfig?.hooks.on_error) {
				runHooks(hooksConfig.hooks.on_error, agentDir, {
					event: "on_error",
					session_id: sessionId,
					error: err.message,
				}).catch(() => {});
			}
			throw err;
		} finally {
			if (localSession) {
				console.log(dim("Finalizing session..."));
				localSession.finalize();
			}
			if (sandboxCtx) {
				console.log(dim("Stopping sandbox..."));
				await sandboxCtx.gitMachine.stop();
			}
		}
		return;
	}

	// REPL mode
	const rl = createInterface({
		input: process.stdin,
		output: process.stdout,
	});

	const ask = (): void => {
		rl.question(green("→ "), async (input) => {
			const trimmed = input.trim();

			if (!trimmed) {
				ask();
				return;
			}

			if (trimmed === "/quit" || trimmed === "/exit") {
				rl.close();
				if (localSession) {
					console.log(dim("Finalizing session..."));
					localSession.finalize();
				}
				await stopSandbox();
				process.exit(0);
			}

			if (trimmed === "/memory") {
				try {
					const mem = await readFile(join(dir, "memory/MEMORY.md"), "utf-8");
					console.log(dim("--- memory ---"));
					console.log(mem.trim() || "(empty)");
					console.log(dim("--- end ---"));
				} catch {
					console.log(dim("(no memory file)"));
				}
				ask();
				return;
			}

			if (trimmed === "/skills") {
				// Refresh skills to pick up any newly learned ones
				const currentSkills = await refreshSkills(dir);
				if (currentSkills.length === 0) {
					console.log(dim("No skills installed."));
				} else {
					for (const s of currentSkills) {
						const conf = s.confidence !== undefined ? dim(` [confidence: ${s.confidence}]`) : "";
						console.log(`  ${bold(s.name)} — ${dim(s.description)}${conf}`);
					}
				}
				ask();
				return;
			}

			if (trimmed === "/tasks") {
				try {
					const tasksRaw = await readFile(join(gitagentDir, "learning", "tasks.json"), "utf-8");
					const tasksData = JSON.parse(tasksRaw);
					const active = (tasksData.tasks || []).filter((t: any) => t.status === "active");
					if (active.length === 0) {
						console.log(dim("No active tasks."));
					} else {
						for (const t of active) {
							console.log(`  ${bold(t.id.slice(0, 8))} — ${t.objective} (${t.steps.length} steps, attempt #${t.attempts})`);
						}
					}
				} catch {
					console.log(dim("No tasks recorded yet."));
				}
				ask();
				return;
			}

			if (trimmed === "/learned") {
				const currentSkills = await refreshSkills(dir);
				const learned = currentSkills.filter((s) => s.confidence !== undefined);
				if (learned.length === 0) {
					console.log(dim("No learned skills yet."));
				} else {
					for (const s of learned) {
						const usage = s.usage_count ?? 0;
						const ratio = `${s.success_count ?? 0}/${(s.success_count ?? 0) + (s.failure_count ?? 0)}`;
						console.log(`  ${bold(s.name)} — confidence: ${s.confidence}, usage: ${usage}, success: ${ratio}`);
					}
				}
				ask();
				return;
			}

			if (trimmed === "/plugins") {
				if (loaded.plugins.length === 0) {
					console.log(dim("No plugins loaded."));
				} else {
					for (const p of loaded.plugins) {
						const toolCount = p.tools.length + p.programmaticTools.length;
						const info = [
							toolCount > 0 ? `${toolCount} tools` : null,
							p.skills.length > 0 ? `${p.skills.length} skills` : null,
							p.hooks ? "hooks" : null,
							p.promptAddition ? "prompt" : null,
						].filter(Boolean).join(", ");
						console.log(`  ${bold(p.manifest.id)} v${p.manifest.version} — ${dim(p.manifest.description)}`);
						if (info) console.log(`    ${dim(`provides: ${info}`)}`);
					}
				}
				ask();
				return;
			}

			// Skill expansion: /skill:name [args]
			let promptText = trimmed;
			if (trimmed.startsWith("/skill:")) {
				const result = await expandSkillCommand(trimmed, skills);
				if (result) {
					console.log(dim(`▶ loading skill: ${result.skillName}`));
					promptText = result.expanded;
				} else {
					const requested = trimmed.match(/^\/skill:([a-z0-9-]*)/)?.[1] || "?";
					console.error(red(`Unknown skill: ${requested}`));
					ask();
					return;
				}
			}

			try {
				await agent.prompt(promptText);
			} catch (err: any) {
				console.error(red(`Error: ${err.message}`));
				auditLogger?.logError(err.message).catch(() => {});
				// Fire on_error hooks
				if (hooksConfig?.hooks.on_error) {
					runHooks(hooksConfig.hooks.on_error, agentDir, {
						event: "on_error",
						session_id: sessionId,
						error: err.message,
					}).catch(() => {});
				}
			}

			ask();
		});
	};

	// Sandbox cleanup helper
	const stopSandbox = async () => {
		if (sandboxCtx) {
			console.log(dim("Stopping sandbox..."));
			await sandboxCtx.gitMachine.stop();
		}
	};

	// Handle Ctrl+C during streaming
	rl.on("SIGINT", () => {
		if (agent.state.isStreaming) {
			agent.abort();
		} else {
			console.log("\nBye!");
			rl.close();
			if (localSession) {
				try { localSession.finalize(); } catch { /* best-effort */ }
			}
			stopSandbox().finally(() => process.exit(0));
		}
	});

	ask();
}

main().catch((err) => {
	console.error(red(`Fatal: ${err.message}`));
	process.exit(1);
});
