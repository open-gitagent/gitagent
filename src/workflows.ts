import { readFile, readdir, stat, writeFile, unlink } from "fs/promises";
import { join } from "path";
import { mkdirSync } from "fs";
import yaml from "js-yaml";

export interface SkillFlowStep {
	/** Optional snake_case id other steps reference via depends_on. */
	id?: string;
	skill: string;
	prompt: string;
	channel?: string;
	/** Ids of steps that must complete first. Kept in sync with workflow.schema.json. */
	depends_on?: string[];
	/** Pause for human approval before running this step. */
	requires_approval?: boolean;
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

const KEBAB_RE = /^[a-z0-9]+(-[a-z0-9]+)*$/;

/**
 * Normalize raw YAML steps into SkillFlowStep. Every field declared in
 * spec/schemas/workflow.schema.json is carried through — dropping `id` or
 * `depends_on` here would silently ignore ordering the author declared.
 */
function normalizeSteps(rawSteps: any[]): SkillFlowStep[] {
	return rawSteps.map((s: any) => ({
		...(s?.id ? { id: String(s.id) } : {}),
		skill: String(s?.skill || ""),
		prompt: String(s?.prompt || ""),
		...(s?.channel ? { channel: String(s.channel) } : {}),
		...(Array.isArray(s?.depends_on) ? { depends_on: s.depends_on.map((d: any) => String(d)) } : {}),
		...(s?.requires_approval != null ? { requires_approval: Boolean(s.requires_approval) } : {}),
	}));
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
							steps: normalizeSteps(data.steps as any[]),
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

let warnedLegacyFlowPath = false;

/**
 * Load a flow by name from `<agentDir>/workflows/<flowName>.yaml`.
 *
 * The path is built here rather than accepted from the caller so the function
 * owns its own safety: `flowName` must be kebab-case, which rules out traversal
 * segments and absolute paths no matter how the name reached this code.
 */
export async function loadFlowDefinition(agentDir: string, flowName: string): Promise<SkillFlowDefinition>;
/**
 * Legacy single-argument form, kept so callers written against the previous
 * signature (e.g. `@open-gitagent/voice`) keep working. It reads the path as
 * given and therefore provides no containment — migrate to
 * `loadFlowDefinition(agentDir, flowName)`.
 *
 * @deprecated Pass `(agentDir, flowName)` instead.
 */
export async function loadFlowDefinition(filePath: string): Promise<SkillFlowDefinition>;
export async function loadFlowDefinition(agentDirOrPath: string, flowName?: string): Promise<SkillFlowDefinition> {
	// Which form was used is decided by arity, not by inspecting the string: a
	// missing second argument would otherwise stringify to "undefined", which
	// passes KEBAB_RE and fails later as a confusing ENOENT.
	let filePath: string;
	if (flowName === undefined) {
		if (!warnedLegacyFlowPath) {
			warnedLegacyFlowPath = true;
			console.warn(
				"[gitagent] loadFlowDefinition(filePath) is deprecated — call loadFlowDefinition(agentDir, flowName) so the path is validated and built internally.",
			);
		}
		filePath = agentDirOrPath;
	} else {
		if (!KEBAB_RE.test(flowName)) {
			throw new Error(`Invalid flow name "${flowName}": must be kebab-case (e.g. my-flow-name)`);
		}
		filePath = join(agentDirOrPath, "workflows", `${flowName}.yaml`);
	}
	const raw = await readFile(filePath, "utf-8");
	const data = yaml.load(raw);

	// yaml.load returns null for an empty file and a string/number for a
	// single scalar document — both need a clearer error than "missing name".
	if (data === null || typeof data !== "object" || Array.isArray(data)) {
		throw new Error("Invalid flow definition: file must be a YAML mapping");
	}
	const doc = data as Record<string, any>;
	if (!doc.name || !Array.isArray(doc.steps)) {
		throw new Error("Invalid flow definition: missing name or steps");
	}

	const steps = normalizeSteps(doc.steps);
	const badStep = steps.findIndex((s) => !s.skill.trim());
	if (badStep !== -1) {
		throw new Error(`Invalid flow definition: step[${badStep}] has an empty skill`);
	}

	// Steps execute in declaration order, so every depends_on must name a
	// preceding step. This also rejects self-references and cycles, and keeps
	// declared dependencies from being silently ignored at run time.
	const available = new Set<string>();
	steps.forEach((s, i) => {
		for (const dep of s.depends_on ?? []) {
			if (!available.has(dep)) {
				throw new Error(
					`Invalid flow definition: step[${i}] depends_on "${dep}", which is not the id of a preceding step`,
				);
			}
		}
		if (s.id) available.add(s.id);
	});

	return {
		name: String(doc.name),
		description: String(doc.description || ""),
		steps,
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
		steps: flow.steps.map((s) => ({
			...(s.id ? { id: s.id } : {}),
			skill: s.skill,
			prompt: s.prompt,
			...(s.channel ? { channel: s.channel } : {}),
			...(s.depends_on?.length ? { depends_on: s.depends_on } : {}),
			...(s.requires_approval != null ? { requires_approval: s.requires_approval } : {}),
		})),
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
