#!/usr/bin/env node

import { createInterface } from "readline";
import { Agent } from "@mariozechner/pi-agent-core";
import type { AgentEvent, AgentTool } from "@mariozechner/pi-agent-core";
import { loadAgent } from "./loader.js";
import { createBuiltinTools } from "./tools/index.js";
import { createSandboxContext } from "./sandbox.js";
import type { SandboxContext, SandboxConfig } from "./sandbox.js";
import { expandSkillCommand } from "./skills.js";
import { loadHooksConfig, runHooks, wrapToolWithHooks } from "./hooks.js";
import type { HooksConfig } from "./hooks.js";
import { loadDeclarativeTools } from "./tool-loader.js";
import { AuditLogger, isAuditEnabled } from "./audit.js";
import { formatComplianceWarnings } from "./compliance.js";
import { readFile, mkdir, writeFile, stat, access } from "fs/promises";
import { join, resolve } from "path";
import { execSync } from "child_process";
import { initLocalSession } from "./session.js";
import type { LocalSession } from "./session.js";
import { startVoiceServer } from "./voice/server.js";

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
	voice?: boolean;
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
	let voice = false;

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
				voice = true;
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
		const apiKey = process.env.OPENAI_API_KEY;
		if (!apiKey) {
			console.error(red("Error: OPENAI_API_KEY is required for --voice mode"));
			process.exit(1);
		}

		const cleanup = await startVoiceServer({
			adapter: "openai-realtime",
			adapterConfig: { apiKey, voice: "alloy" },
			agentDir: dir,
			model,
			env,
		});

		process.on("SIGINT", async () => {
			console.log("\nDisconnecting...");
			await cleanup();
			process.exit(0);
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

	// Load hooks config
	const hooksConfig = await loadHooksConfig(agentDir);

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

	// Build tools — built-in + declarative
	let tools: AgentTool<any>[] = createBuiltinTools({
		dir,
		timeout: manifest.runtime.timeout,
		sandbox: sandboxCtx,
	});

	// Load declarative tools from tools/*.yaml (Phase 2.2)
	const declarativeTools = await loadDeclarativeTools(agentDir);
	tools = [...tools, ...declarativeTools];

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

	let headerLines = 0;
	console.log(bold(`${manifest.name} v${manifest.version}`)); headerLines++;
	console.log(dim(`Model: ${loaded.model.provider}:${loaded.model.id}`)); headerLines++;
	const allToolNames = tools.map((t) => t.name);
	console.log(dim(`Tools: ${allToolNames.join(", ")}`)); headerLines++;
	if (skills.length > 0) {
		console.log(dim(`Skills: ${skills.map((s) => s.name).join(", ")}`)); headerLines++;
	}
	if (loaded.workflows.length > 0) {
		console.log(dim(`Workflows: ${loaded.workflows.map((w) => w.name).join(", ")}`)); headerLines++;
	}
	if (loaded.subAgents.length > 0) {
		console.log(dim(`Agents: ${loaded.subAgents.map((a) => a.name).join(", ")}`)); headerLines++;
	}
	console.log(dim('Type /skills to list skills, /memory to view memory, /quit to exit\n')); headerLines += 2;

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

	// Sandbox cleanup helper
	const stopSandbox = async () => {
		if (sandboxCtx) {
			console.log(dim("Stopping sandbox..."));
			await sandboxCtx.gitMachine.stop();
		}
	};

	// ── Unified REPL with fixed input line ───────────────────────
	// Always in raw mode. Scroll region keeps streaming output in
	// the top area; input line is always visible at the bottom.
	// No readline — we manage everything ourselves.

	let inputBuffer = "";
	let isRunning = false; // true while agent.prompt() is in progress
	let queueText = ""; // pending steer message shown on the queue line

	const rows = () => process.stdout.rows || 24;
	const cols = () => process.stdout.columns || 80;

	// Layout (bottom 3 rows are reserved):
	//   rows 1..(r-3)  — scroll region (streaming output)
	//   row  r-2       — queue line (pending steer message)
	//   row  r-1       — separator
	//   row  r         — input line

	const drawQueueLine = () => {
		const r = rows();
		if (queueText) {
			process.stdout.write(`\x1b7\x1b[${r - 2};1H\x1b[2K\x1b[33m⤷ ${queueText}\x1b[0m\x1b8`);
		} else {
			process.stdout.write(`\x1b7\x1b[${r - 2};1H\x1b[2K\x1b8`);
		}
	};

	const drawSeparator = () => {
		const r = rows();
		process.stdout.write(`\x1b7\x1b[${r - 1};1H\x1b[2K\x1b[2m${"─".repeat(cols())}\x1b[0m\x1b8`);
	};

	const drawInputLine = () => {
		const r = rows();
		const prompt = isRunning ? `\x1b[2m⤷\x1b[0m ` : `\x1b[32m→\x1b[0m `;
		// Fake block cursor (inverse video space) shows typing position
		const cursor = `\x1b[7m \x1b[0m`;
		// Save/restore keeps the real (hidden) cursor in the scroll region
		process.stdout.write(`\x1b7\x1b[${r};1H\x1b[2K${prompt}${inputBuffer}${cursor}\x1b8`);
	};

	const initUI = (cursorRow: number) => {
		if (!process.stdout.isTTY) return;
		const r = rows();
		// Hide real cursor — we use a fake cursor on the input line
		process.stdout.write(`\x1b[?25l`);
		// Set scroll region: rows 1 to (r-3), leaving 3 rows for queue + separator + input
		process.stdout.write(`\x1b[1;${r - 3}r`);
		drawQueueLine();
		drawSeparator();
		drawInputLine();
		// Reposition cursor after header so new output doesn't overwrite it
		// (setting scroll region resets cursor to row 1)
		process.stdout.write(`\x1b[${cursorRow};1H`);
	};

	const cleanupUI = () => {
		if (!process.stdout.isTTY) return;
		// Show real cursor again
		process.stdout.write(`\x1b[?25h`);
		// Reset scroll region, clear bottom lines
		const r = rows();
		process.stdout.write(`\x1b[${r - 2};1H\x1b[2K`);
		process.stdout.write(`\x1b[${r - 1};1H\x1b[2K`);
		process.stdout.write(`\x1b[${r};1H\x1b[2K`);
		process.stdout.write(`\x1b[r`);
	};

	// Handle a command (when agent is idle)
	const handleCommand = async (text: string) => {
		if (text === "/quit" || text === "/exit") {
			cleanupUI();
			if (process.stdin.isTTY) process.stdin.setRawMode(false);
			console.log("Bye!");
			if (localSession) {
				try { localSession.finalize(); } catch { /* best-effort */ }
			}
			await stopSandbox();
			process.exit(0);
		}

		if (text === "/memory") {
			try {
				const mem = await readFile(join(dir, "memory/MEMORY.md"), "utf-8");
				process.stdout.write(dim("--- memory ---\n"));
				process.stdout.write((mem.trim() || "(empty)") + "\n");
				process.stdout.write(dim("--- end ---\n"));
			} catch {
				process.stdout.write(dim("(no memory file)\n"));
			}
			drawInputLine();
			return;
		}

		if (text === "/skills") {
			if (skills.length === 0) {
				process.stdout.write(dim("No skills installed.\n"));
			} else {
				for (const s of skills) {
					process.stdout.write(`  ${bold(s.name)} — ${dim(s.description)}\n`);
				}
			}
			drawInputLine();
			return;
		}

		// Skill expansion
		let promptText = text;
		if (text.startsWith("/skill:")) {
			const result = await expandSkillCommand(text, skills);
			if (result) {
				process.stdout.write(dim(`▶ loading skill: ${result.skillName}\n`));
				promptText = result.expanded;
			} else {
				const requested = text.match(/^\/skill:([a-z0-9-]*)/)?.[1] || "?";
				process.stdout.write(red(`Unknown skill: ${requested}\n`));
				drawInputLine();
				return;
			}
		}

		// Send prompt to agent
		isRunning = true;
		drawInputLine(); // switch prompt from → to ⤷
		process.stdout.write(`\n${green(`→ ${text}`)}\n`); // blank line + echo user input

		try {
			await agent.prompt(promptText);
		} catch (err: any) {
			process.stdout.write(red(`Error: ${err.message}\n`));
			auditLogger?.logError(err.message).catch(() => {});
			if (hooksConfig?.hooks.on_error) {
				runHooks(hooksConfig.hooks.on_error, agentDir, {
					event: "on_error",
					session_id: sessionId,
					error: err.message,
				}).catch(() => {});
			}
		}

		isRunning = false;
		queueText = "";
		drawQueueLine(); // clear steer message
		drawInputLine(); // switch prompt back from ⤷ to →
	};

	const onKeystroke = (key: Buffer) => {
		const ch = key.toString("utf-8");
		const code = key[0];

		// Ctrl+C
		if (code === 3) {
			if (isRunning) {
				inputBuffer = "";
				drawInputLine();
				agent.abort();
			} else {
				cleanupUI();
				if (process.stdin.isTTY) process.stdin.setRawMode(false);
				console.log("\nBye!");
				if (localSession) {
					try { localSession.finalize(); } catch { /* best-effort */ }
				}
				stopSandbox().finally(() => process.exit(0));
			}
			return;
		}

		// Enter
		if (code === 13 || code === 10) {
			const text = inputBuffer.trim();
			inputBuffer = "";
			drawInputLine();
			if (!text) return;

			if (isRunning) {
				// Show on queue line until agent picks it up
				queueText = text;
				drawQueueLine();
				agent.steer({
					role: "user",
					content: text,
					timestamp: Date.now(),
				});
			} else {
				// Send as new prompt
				handleCommand(text);
			}
			return;
		}

		// Backspace
		if (code === 127 || code === 8) {
			inputBuffer = inputBuffer.slice(0, -1);
			drawInputLine();
			return;
		}

		// Printable characters
		if (code >= 32) {
			inputBuffer += ch;
			drawInputLine();
		}
	};

	// Handle terminal resize — redraw all fixed lines
	process.stdout.on("resize", () => {
		const r = rows();
		process.stdout.write(`\x1b[1;${r - 3}r`);
		drawQueueLine();
		drawSeparator();
		drawInputLine();
	});

	// Echo steer messages in scroll region when agent picks them up
	agent.subscribe((event) => {
		if (event.type === "message_start") {
			const msg = event.message as any;
			if (msg?.role === "user" && queueText) {
				// Agent picked up the steer — echo it in the scroll region
				process.stdout.write(`\x1b[33m⤷ ${queueText}\x1b[0m\n`);
				queueText = "";
				drawQueueLine();
			}
		}
	});

	// Start the UI
	initUI(headerLines + 1);
	if (process.stdin.isTTY) {
		process.stdin.setRawMode(true);
		process.stdin.resume();
		process.stdin.on("data", onKeystroke);
	}
}

main().catch((err) => {
	console.error(red(`Fatal: ${err.message}`));
	process.exit(1);
});
