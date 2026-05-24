import { readFile, readdir, stat } from "fs/promises";
import { join } from "path";
import yaml from "js-yaml";

export interface SubAgentMetadata {
	name: string;
	description: string;
	type: "directory" | "file";
	path: string;
}

function parseFrontmatter(content: string): { frontmatter: Record<string, any>; body: string } {
	const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
	if (!match) {
		return { frontmatter: {}, body: content };
	}
	const frontmatter = yaml.load(match[1]) as Record<string, any>;
	return { frontmatter, body: match[2] };
}

export async function discoverSubAgents(agentDir: string): Promise<SubAgentMetadata[]> {
	const agentsDir = join(agentDir, "agents");

	try {
		const s = await stat(agentsDir);
		if (!s.isDirectory()) return [];
	} catch {
		return [];
	}

	const entries = await readdir(agentsDir, { withFileTypes: true });
	const agents: SubAgentMetadata[] = [];

	for (const entry of entries) {
		const entryPath = join(agentsDir, entry.name);

		if (entry.isDirectory()) {
			// Directory form: agents/<name>/agent.yaml
			const agentYamlPath = join(entryPath, "agent.yaml");
			try {
				const raw = await readFile(agentYamlPath, "utf-8");
				const data = yaml.load(raw) as Record<string, any>;
				if (data?.name && data?.description) {
					agents.push({
						name: data.name,
						description: data.description,
						type: "directory",
						path: `agents/${entry.name}`,
					});
				}
			} catch {
				// Skip directories without valid agent.yaml
			}
		} else if (entry.name.endsWith(".md") && entry.isFile()) {
			// File form: agents/<name>.md
			try {
				const raw = await readFile(entryPath, "utf-8");
				const { frontmatter } = parseFrontmatter(raw);
				const name = (frontmatter.name as string) || entry.name.replace(/\.md$/, "");
				const description = (frontmatter.description as string) || "";
				if (description) {
					agents.push({
						name,
						description,
						type: "file",
						path: `agents/${entry.name}`,
					});
				}
			} catch {
				// Skip unreadable files
			}
		}
	}

	return agents.sort((a, b) => a.name.localeCompare(b.name));
}

export function formatSubAgentsForPrompt(agents: SubAgentMetadata[]): string {
	if (agents.length === 0) return "";

	const entries = agents
		.map(
			(a) =>
				`<agent>\n<name>${a.name}</name>\n<description>${a.description}</description>\n<type>${a.type}</type>\n<path>${a.path}</path>\n</agent>`,
		)
		.join("\n");

	return `# Sub-Agents

<available_agents>
${entries}
</available_agents>

To delegate to a sub-agent, use the \`cli\` tool to run: \`gitagent --dir ${"{agent_path}"} -p "task description"\`
For file-based agents, use the \`read\` tool to load their instructions.`;
}
