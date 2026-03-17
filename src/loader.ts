import { readFile, mkdir, writeFile } from "fs/promises";
import { join } from "path";
import { randomUUID } from "crypto";
import { execSync } from "child_process";
import { getModel } from "@mariozechner/pi-ai";
import type { Model } from "@mariozechner/pi-ai";
import yaml from "js-yaml";
import { discoverSkills, formatSkillsForPrompt } from "./skills.js";
import type { SkillMetadata } from "./skills.js";
import { loadKnowledge, formatKnowledgeForPrompt } from "./knowledge.js";
import type { LoadedKnowledge } from "./knowledge.js";
import { discoverWorkflows, formatWorkflowsForPrompt } from "./workflows.js";
import type { WorkflowMetadata } from "./workflows.js";
import { loadEnvConfig } from "./config.js";
import type { EnvConfig } from "./config.js";
import { discoverSubAgents, formatSubAgentsForPrompt } from "./agents.js";
import type { SubAgentMetadata } from "./agents.js";
import { loadExamples, formatExamplesForPrompt } from "./examples.js";
import type { ExampleEntry } from "./examples.js";
import { validateCompliance, loadComplianceContext, formatComplianceWarnings } from "./compliance.js";
import type { ComplianceWarning } from "./compliance.js";
import { discoverAndLoadPlugins } from "./plugins.js";
import type { LoadedPlugin } from "./plugin-types.js";
import type { PluginConfig } from "./plugin-types.js";

export interface AgentManifest {
	spec_version: string;
	name: string;
	version: string;
	description: string;
	author?: string;
	license?: string;
	tags?: string[];
	metadata?: Record<string, string | number | boolean>;
	model: {
		preferred: string;
		fallback: string[];
		constraints?: {
			temperature?: number;
			max_tokens?: number;
			top_p?: number;
			top_k?: number;
			stop_sequences?: string[];
		};
	};
	tools: string[];
	skills?: string[];
	runtime: {
		max_turns: number;
		timeout?: number;
	};
	extends?: string;
	dependencies?: Array<{ name: string; source: string; version: string; mount: string }>;
	agents?: Record<string, any>;
	delegation?: { mode: "auto" | "explicit" | "router"; router?: string };
	compliance?: Record<string, any>;
	plugins?: Record<string, PluginConfig>;
}

async function readFileOr(path: string, fallback: string): Promise<string> {
	try {
		return await readFile(path, "utf-8");
	} catch {
		return fallback;
	}
}

function parseModelString(modelStr: string): { provider: string; modelId: string } {
	const colonIndex = modelStr.indexOf(":");
	if (colonIndex === -1) {
		throw new Error(
			`Invalid model format: "${modelStr}". Expected "provider:model" (e.g., "anthropic:claude-sonnet-4-5-20250929")`,
		);
	}
	return {
		provider: modelStr.slice(0, colonIndex),
		modelId: modelStr.slice(colonIndex + 1),
	};
}

async function ensureGitagentDir(agentDir: string): Promise<string> {
	const gitagentDir = join(agentDir, ".gitagent");
	await mkdir(gitagentDir, { recursive: true });

	// Ensure .gitagent is in .gitignore
	const gitignorePath = join(agentDir, ".gitignore");
	try {
		const gitignore = await readFile(gitignorePath, "utf-8");
		if (!gitignore.includes(".gitagent")) {
			await writeFile(gitignorePath, gitignore.trimEnd() + "\n.gitagent/\n", "utf-8");
		}
	} catch {
		// No .gitignore or can't read — that's fine
	}

	return gitagentDir;
}

async function writeSessionState(gitagentDir: string): Promise<string> {
	const sessionId = randomUUID();
	const state = {
		session_id: sessionId,
		started_at: new Date().toISOString(),
	};
	await writeFile(join(gitagentDir, "state.json"), JSON.stringify(state, null, 2), "utf-8");
	return sessionId;
}

export interface LoadedAgent {
	systemPrompt: string;
	manifest: AgentManifest;
	model: Model<any>;
	skills: SkillMetadata[];
	knowledge: LoadedKnowledge;
	workflows: WorkflowMetadata[];
	subAgents: SubAgentMetadata[];
	examples: ExampleEntry[];
	envConfig: EnvConfig;
	sessionId: string;
	agentDir: string;
	gitagentDir: string;
	complianceWarnings: ComplianceWarning[];
	plugins: LoadedPlugin[];
}

function deepMerge(base: Record<string, any>, override: Record<string, any>): Record<string, any> {
	const result = { ...base };
	for (const key of Object.keys(override)) {
		if (
			result[key] &&
			typeof result[key] === "object" &&
			!Array.isArray(result[key]) &&
			typeof override[key] === "object" &&
			!Array.isArray(override[key])
		) {
			result[key] = deepMerge(result[key], override[key]);
		} else {
			result[key] = override[key];
		}
	}
	return result;
}

async function resolveInheritance(
	manifest: AgentManifest,
	agentDir: string,
	gitagentDir: string,
): Promise<{ manifest: AgentManifest; parentRules: string }> {
	if (!manifest.extends) {
		return { manifest, parentRules: "" };
	}

	const depsDir = join(gitagentDir, "deps");
	await mkdir(depsDir, { recursive: true });

	// Clone parent into .gitagent/deps/
	const parentName = manifest.extends.split("/").pop()?.replace(/\.git$/, "") || "parent";
	const parentDir = join(depsDir, parentName);

	try {
		execSync(`git clone --depth 1 "${manifest.extends}" "${parentDir}" 2>/dev/null || true`, {
			cwd: agentDir,
			stdio: "pipe",
		});
	} catch {
		// Clone failed, continue without parent
		return { manifest, parentRules: "" };
	}

	// Load parent manifest
	let parentManifest: AgentManifest;
	try {
		const parentRaw = await readFile(join(parentDir, "agent.yaml"), "utf-8");
		parentManifest = yaml.load(parentRaw) as AgentManifest;
	} catch {
		return { manifest, parentRules: "" };
	}

	// Deep merge: child wins
	const merged = deepMerge(parentManifest as any, manifest as any) as AgentManifest;

	// Tools and skills: union, child shadows
	if (parentManifest.tools && manifest.tools) {
		const toolSet = new Set([...parentManifest.tools, ...manifest.tools]);
		merged.tools = [...toolSet];
	}

	// Load parent RULES.md for appending (union)
	const parentRules = await readFileOr(join(parentDir, "RULES.md"), "");

	return { manifest: merged, parentRules };
}

async function resolveDependencies(
	manifest: AgentManifest,
	agentDir: string,
	gitagentDir: string,
): Promise<void> {
	if (!manifest.dependencies || manifest.dependencies.length === 0) return;

	const depsDir = join(gitagentDir, "deps");
	await mkdir(depsDir, { recursive: true });

	for (const dep of manifest.dependencies) {
		const depDir = join(depsDir, dep.name);
		try {
			execSync(
				`git clone --depth 1 --branch "${dep.version}" "${dep.source}" "${depDir}" 2>/dev/null || true`,
				{ cwd: agentDir, stdio: "pipe" },
			);
		} catch {
			// Clone failed, skip this dependency
		}
	}
}

export async function loadAgent(
	agentDir: string,
	modelFlag?: string,
	envFlag?: string,
): Promise<LoadedAgent> {
	// Parse agent.yaml
	const manifestRaw = await readFile(join(agentDir, "agent.yaml"), "utf-8");
	let manifest = yaml.load(manifestRaw) as AgentManifest;

	// Load environment config
	const envConfig = await loadEnvConfig(agentDir, envFlag);

	// Ensure .gitagent/ directory and write session state
	const gitagentDir = await ensureGitagentDir(agentDir);
	const sessionId = await writeSessionState(gitagentDir);

	// Resolve inheritance (Phase 2.4)
	let parentRules = "";
	if (manifest.extends) {
		const resolved = await resolveInheritance(manifest, agentDir, gitagentDir);
		manifest = resolved.manifest;
		parentRules = resolved.parentRules;
	}

	// Resolve dependencies (Phase 2.5)
	await resolveDependencies(manifest, agentDir, gitagentDir);

	// Discover and load plugins
	const plugins = await discoverAndLoadPlugins(agentDir, gitagentDir, manifest.plugins);

	// Validate compliance (Phase 3)
	const complianceWarnings = validateCompliance(manifest);

	// Read identity files
	const soul = await readFileOr(join(agentDir, "SOUL.md"), "");
	const rules = await readFileOr(join(agentDir, "RULES.md"), "");
	const duties = await readFileOr(join(agentDir, "DUTIES.md"), "");
	const agentsMd = await readFileOr(join(agentDir, "AGENTS.md"), "");

	// Build system prompt
	const parts: string[] = [];

	parts.push(`# ${manifest.name} v${manifest.version}\n${manifest.description}`);

	if (soul) parts.push(soul);
	if (rules) parts.push(rules);
	if (parentRules) parts.push(parentRules); // Append parent rules (union)
	if (duties) parts.push(duties);
	if (agentsMd) parts.push(agentsMd);

	parts.push(
		`# Memory\n\nYou have a memory file at memory/MEMORY.md. Use the \`memory\` tool to load and save memories. Each save creates a git commit, so your memory has full history. You can also use the \`cli\` tool to run git commands for deeper memory inspection (git log, git diff, git show).\n\nYour memories define who you are. When you have none, you are newly awakened — curious and eager to understand the person you're talking to. As memories grow, so do you. Save memories proactively when you learn something meaningful about the user.`,
	);

	// Discover and load knowledge
	const knowledge = await loadKnowledge(agentDir);
	const knowledgeBlock = formatKnowledgeForPrompt(knowledge);
	if (knowledgeBlock) parts.push(knowledgeBlock);

	// Discover skills (filtered by manifest.skills if set)
	let skills = await discoverSkills(agentDir);
	if (manifest.skills && manifest.skills.length > 0) {
		const allowed = new Set(manifest.skills);
		skills = skills.filter((s) => allowed.has(s.name));
	}
	// Plugin skills are merged without filtering — plugins are explicitly
	// enabled in agent.yaml, so their skills are considered trusted.
	for (const plugin of plugins) {
		skills = [...skills, ...plugin.skills];
	}
	const skillsBlock = formatSkillsForPrompt(skills);
	if (skillsBlock) parts.push(skillsBlock);

	// Discover workflows
	const workflows = await discoverWorkflows(agentDir);
	const workflowsBlock = formatWorkflowsForPrompt(workflows);
	if (workflowsBlock) parts.push(workflowsBlock);

	// Discover sub-agents (Phase 2.1)
	const subAgents = await discoverSubAgents(agentDir);
	const subAgentsBlock = formatSubAgentsForPrompt(subAgents);
	if (subAgentsBlock) parts.push(subAgentsBlock);

	// Load examples (Phase 2.3)
	const examples = await loadExamples(agentDir);
	const examplesBlock = formatExamplesForPrompt(examples);
	if (examplesBlock) parts.push(examplesBlock);

	// Append plugin prompt additions
	for (const plugin of plugins) {
		if (plugin.promptAddition) {
			parts.push(`# Plugin: ${plugin.manifest.name}\n\n${plugin.promptAddition}`);
		}
	}

	// Load compliance context (Phase 3)
	const complianceBlock = await loadComplianceContext(agentDir);
	if (complianceBlock) parts.push(complianceBlock);

	// Workspace directory — all generated files go here
	parts.push(`# Workspace Directory

Your working directory is \`${agentDir}\`.

When creating files (documents, PDFs, images, spreadsheets, code output, exports, assets, etc.), write them to the \`workspace/\` directory by default.
- Example: \`workspace/report.pdf\`, \`workspace/chart.png\`, \`workspace/data.csv\`
- The \`workspace/\` directory is the designated output folder for generated artifacts
- If the user explicitly specifies a path (e.g. "create ~/notes/todo.md"), use the path they requested
- This rule applies to ALL channels: voice, chat, Telegram, WhatsApp`);

	// Task learning & skill discovery
	parts.push(`# Task Learning & Skill Discovery

You have an intelligent learning system. For ANY task the user gives you:

1. FIRST: Call \`task_tracker\` action "begin" with your objective — this searches for existing skills
2. If a matching skill is found, you MUST load and follow its instructions BEFORE doing anything else
3. Call \`task_tracker\` action "update" after each significant step
4. Call \`task_tracker\` action "end" to report the outcome (success/failure/partial)

IMPORTANT: Do NOT skip step 1. Even for tasks that seem simple, always check for skills first.
Skills encode tested approaches and handle edge cases you might miss with ad-hoc solutions.

On SUCCESS:
- Call \`skill_learner\` action "evaluate" to check if this approach is worth saving
- If worthy, call \`skill_learner\` action "crystallize" to save it as a reusable skill
- The skill will be available in future sessions via /skill:<name>

On FAILURE:
- Record why it failed. Try a different approach.
- Failed approaches become negative examples — they won't be repeated

If you used an existing skill, report it via skill_used so confidence adjusts based on the outcome.
Do NOT track trivial single-command tasks (e.g. "what time is it"). But DO check skills for any task that involves creating, building, or modifying something.`);

	const systemPrompt = parts.join("\n\n");

	// Resolve model — env config model_override > CLI flag > manifest preferred
	const modelStr = envConfig.model_override || modelFlag || manifest.model.preferred;
	if (!modelStr) {
		throw new Error(
			'No model configured. Either:\n  - Set model.preferred in agent.yaml (e.g., "anthropic:claude-sonnet-4-5-20250929")\n  - Pass --model provider:model on the command line',
		);
	}

	const { provider, modelId } = parseModelString(modelStr);
	const model = getModel(provider as any, modelId as any);

	return {
		systemPrompt,
		manifest,
		model,
		skills,
		knowledge,
		workflows,
		subAgents,
		examples,
		envConfig,
		sessionId,
		agentDir,
		gitagentDir,
		complianceWarnings,
		plugins,
	};
}
