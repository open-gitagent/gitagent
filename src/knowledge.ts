import { readFile, readdir } from "fs/promises";
import { join, resolve, relative, isAbsolute, sep } from "path";
import yaml from "js-yaml";

export interface KnowledgeEntry {
	path: string;
	tags: string[];
	priority: "high" | "medium" | "low";
	always_load?: boolean;
}

interface KnowledgeIndex {
	entries: KnowledgeEntry[];
}

export interface LoadedKnowledge {
	/** Content from always_load docs, ready to inject into system prompt */
	preloaded: Array<{ path: string; content: string }>;
	/** Entries available on demand via read tool */
	available: KnowledgeEntry[];
}

export async function loadKnowledge(agentDir: string): Promise<LoadedKnowledge> {
	const knowledgeDir = join(agentDir, "knowledge");
	const indexPath = join(knowledgeDir, "index.yaml");

	let raw: string;
	try {
		raw = await readFile(indexPath, "utf-8");
	} catch {
		return { preloaded: [], available: [] };
	}

	const index = yaml.load(raw) as KnowledgeIndex;
	if (!index?.entries || !Array.isArray(index.entries)) {
		return { preloaded: [], available: [] };
	}

	const preloaded: LoadedKnowledge["preloaded"] = [];
	const available: KnowledgeEntry[] = [];

	for (const entry of index.entries) {
		// Reject any entry whose path escapes knowledgeDir — applied to both
		// always_load and on-demand (available) entries, so a malicious
		// index.yaml can't be surfaced for later reads either.
		const resolvedPath = resolve(join(knowledgeDir, entry.path));
		const rel = relative(resolve(knowledgeDir), resolvedPath);
		if (rel === ".." || rel.startsWith(".." + sep) || isAbsolute(rel)) {
			continue; // entry.path escapes knowledgeDir — skip
		}
		if (entry.always_load) {
			try {
				const content = await readFile(resolvedPath, "utf-8");
				preloaded.push({ path: entry.path, content: content.trim() });
			} catch {
				// Skip missing files
			}
		} else {
			available.push(entry);
		}
	}

	return { preloaded, available };
}

export function formatKnowledgeForPrompt(knowledge: LoadedKnowledge): string {
	const parts: string[] = [];

	// Inject always_load content directly
	for (const doc of knowledge.preloaded) {
		parts.push(`<knowledge path="${doc.path}">\n${doc.content}\n</knowledge>`);
	}

	// List available docs for on-demand access
	if (knowledge.available.length > 0) {
		const entries = knowledge.available
			.map((e) => {
				const tags = e.tags.length > 0 ? ` tags="${e.tags.join(",")}"` : "";
				return `<doc path="knowledge/${e.path}" priority="${e.priority}"${tags} />`;
			})
			.join("\n");
		parts.push(
			`<available_knowledge>\n${entries}\n</available_knowledge>\n\nUse the \`read\` tool to load any available knowledge document when needed.`,
		);
	}

	if (parts.length === 0) return "";
	return `# Knowledge\n\n${parts.join("\n\n")}`;
}
