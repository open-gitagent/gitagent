#!/usr/bin/env node

import { createInterface } from "readline";
import { Agent } from "@mariozechner/pi-agent-core";
import type { AgentEvent, AgentTool } from "@mariozechner/pi-agent-core";
import { loadAgent } from "./loader.js";
import { createCliTool } from "./tools/cli.js";
import { createReadTool } from "./tools/read.js";
import { createWriteTool } from "./tools/write.js";
import { createMemoryTool } from "./tools/memory.js";
import { expandSkillCommand } from "./skills.js";
import { loadHooksConfig, runHooks, wrapToolWithHooks } from "./hooks.js";
import type { HooksConfig } from "./hooks.js";
import { loadDeclarativeTools } from "./tool-loader.js";
import { AuditLogger, isAuditEnabled } from "./audit.js";
import { formatComplianceWarnings } from "./compliance.js";
import { readFile, mkdir, writeFile, stat, access } from "fs/promises";
import { join, resolve } from "path";
import { execSync } from "child_process";

// ANSI helpers
const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const bold = (s: string) => `\x1b[1m${s}\x1b[0m`;
const red = (s: string) => `\x1b[31m${s}\x1b[0m`;
const green = (s: string) => `\x1b[32m${s}\x1b[0m`;

function parseArgs(argv: string[]): { model?: string; dir: string; prompt?: string; env?: string } {
	const args = argv.slice(2);
	let model: string | undefined;
	let dir = process.cwd();
	let prompt: string | undefined;
	let env: string | undefined;

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
			default:
				if (!args[i].startsWith("-")) {
					prompt = args[i];
				}
				break;
		}
	}

	return { model, dir, prompt, env };
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
	const { model, dir: rawDir, prompt, env } = parseArgs(process.argv);

	// If no --dir given interactively, ask for it
	let dir = rawDir;
	if (dir === process.cwd() && !prompt) {
		const answer = await askQuestion(green("? ") + bold("Repository path") + dim(" (. for current dir)") + green(": "));
		if (answer) {
			dir = resolve(answer === "." ? process.cwd() : answer);
		}
	}

	// Ensure the target is a valid gitclaw repo
	dir = await ensureRepo(dir, model);

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
	let tools: AgentTool<any>[] = [
		createCliTool(dir, manifest.runtime.timeout),
		createReadTool(dir),
		createWriteTool(dir),
		createMemoryTool(dir),
	];

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
	console.log(dim('Type /skills to list skills, /memory to view memory, /quit to exit\n'));

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
				if (skills.length === 0) {
					console.log(dim("No skills installed."));
				} else {
					for (const s of skills) {
						console.log(`  ${bold(s.name)} — ${dim(s.description)}`);
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

	// Handle Ctrl+C during streaming
	rl.on("SIGINT", () => {
		if (agent.state.isStreaming) {
			agent.abort();
		} else {
			console.log("\nBye!");
			rl.close();
			process.exit(0);
		}
	});

	ask();
}

main().catch((err) => {
	console.error(red(`Fatal: ${err.message}`));
	process.exit(1);
});
