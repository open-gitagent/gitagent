import { readFile, readdir, stat, writeFile, unlink } from "fs/promises";
import { join } from "path";
import { mkdirSync } from "fs";
import yaml from "js-yaml";

export interface SkillFlowStep {
	skill: string;
	prompt: string;
	channel?: string;
}

export interface SkillFlowDefinition {
	name: string;
	description: string;
	steps: SkillFlowStep[];
}

export interface WorkflowMetadata {
	name: string;
	description: string;
	filePath: string;
	format: "yaml" | "markdown";
	type?: "flow" | "basic";
	steps?: SkillFlowStep[];
}

function parseFrontmatter(content: string): { frontmatter: Record<string, any>; body: string } {
	const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
	if (!match) {
		return { frontmatter: {}, body: content };
	}
	const frontmatter = yaml.load(match[1]) as Record<string, any>;
	return { frontmatter, body: match[2] };
}

export async function discoverWorkflows(agentDir: string): Promise<WorkflowMetadata[]> {
	const workflowsDir = join(agentDir, "workflows");

	try {
		const s = await stat(workflowsDir);
		if (!s.isDirectory()) return [];
	} catch {
		return [];
	}

	const entries = await readdir(workflowsDir);
	const workflows: WorkflowMetadata[] = [];

	for (const entry of entries) {
		const filePath = join(workflowsDir, entry);
		const s = await stat(filePath);
		if (!s.isFile()) continue;

		if (entry.endsWith(".yaml") || entry.endsWith(".yml")) {
			try {
				const raw = await readFile(filePath, "utf-8");
				const data = yaml.load(raw) as Record<string, any>;
				if (data?.name) {
					const isFlow = Array.isArray(data.steps) && data.steps.length > 0;
					workflows.push({
						name: data.name,
						description: data.description,
						filePath: `workflows/${entry}`,
						format: "yaml",
						...(isFlow ? {
							type: "flow" as const,
							steps: (data.steps as any[]).map((s: any) => ({
								skill: String(s.skill || ""),
								prompt: String(s.prompt || ""),
								...(s.channel ? { channel: String(s.channel) } : {}),
							})),
						} : { type: "basic" as const }),
					});
				}
			} catch {
				// Skip invalid YAML
			}
		} else if (entry.endsWith(".md")) {
			try {
				const raw = await readFile(filePath, "utf-8");
				const { frontmatter } = parseFrontmatter(raw);
				const name = (frontmatter.name as string) || entry.replace(/\.md$/, "");
				const description = (frontmatter.description as string) || "";
				if (description) {
					workflows.push({
						name,
						description,
						filePath: `workflows/${entry}`,
						format: "markdown",
					});
				}
			} catch {
				// Skip unreadable files
			}
		}
	}

	return workflows.sort((a, b) => a.name.localeCompare(b.name));
}

const KEBAB_RE = /^[a-z0-9]+(-[a-z0-9]+)*$/;

export async function loadFlowDefinition(filePath: string): Promise<SkillFlowDefinition> {
	const raw = await readFile(filePath, "utf-8");
	const data = yaml.load(raw) as Record<string, any>;
	if (!data?.name || !data?.steps || !Array.isArray(data.steps)) {
		throw new Error("Invalid flow definition: missing name or steps");
	}
	return {
		name: data.name,
		description: data.description || "",
		steps: data.steps.map((s: any) => ({
			skill: String(s.skill || ""),
			prompt: String(s.prompt || ""),
			...(s.channel ? { channel: String(s.channel) } : {}),
		})),
	};
}

export async function saveFlowDefinition(agentDir: string, flow: SkillFlowDefinition): Promise<string> {
	if (!KEBAB_RE.test(flow.name)) {
		throw new Error("Flow name must be kebab-case (e.g. my-flow-name)");
	}
	if (!flow.steps || flow.steps.length === 0) {
		throw new Error("Flow must have at least one step");
	}
	const workflowsDir = join(agentDir, "workflows");
	mkdirSync(workflowsDir, { recursive: true });
	const filePath = join(workflowsDir, `${flow.name}.yaml`);
	const content = yaml.dump({
		name: flow.name,
		description: flow.description || "",
		steps: flow.steps.map((s) => ({ skill: s.skill, prompt: s.prompt, ...(s.channel ? { channel: s.channel } : {}) })),
	}, { lineWidth: 120 });
	await writeFile(filePath, content, "utf-8");
	return filePath;
}

export async function deleteFlowDefinition(agentDir: string, name: string): Promise<void> {
	const filePath = join(agentDir, "workflows", `${name}.yaml`);
	await unlink(filePath);
}

export function formatWorkflowsForPrompt(workflows: WorkflowMetadata[]): string {
	if (workflows.length === 0) return "";

	const entries = workflows
		.map(
			(w) =>
				`<workflow>\n<name>${w.name}</name>\n<description>${w.description}</description>\n<path>${w.filePath}</path>${w.type === "flow" ? "\n<type>flow</type>" : ""}\n</workflow>`,
		)
		.join("\n");

	const flowNames = workflows.filter((w) => w.type === "flow").map((w) => w.name);
	const flowNote = flowNames.length > 0
		? `\n\nSkillFlows can be triggered with @flow_name in chat (e.g. ${flowNames.map((n) => "@" + n).join(", ")}).`
		: "";

	return `# Workflows

<available_workflows>
${entries}
</available_workflows>

Use the \`read\` tool to load a workflow's full definition when you need to follow it.${flowNote}`;
}
