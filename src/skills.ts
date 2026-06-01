import { readFile, readdir, stat } from "fs/promises";
import { join } from "path";
import yaml from "js-yaml";

export interface SkillMetadata {
	name: string;
	description: string;
	directory: string;
	filePath: string;
	confidence?: number;
	usage_count?: number;
	success_count?: number;
	failure_count?: number;
	model?: string;
}

export interface ParsedSkill extends SkillMetadata {
	instructions: string;
	hasScripts: boolean;
	hasReferences: boolean;
}

const KEBAB_RE = /^[a-z0-9]+(-[a-z0-9]+)*$/;

function parseFrontmatter(content: string): { frontmatter: Record<string, any>; body: string } {
	const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
	if (!match) {
		return { frontmatter: {}, body: content };
	}
	const frontmatter = yaml.load(match[1]) as Record<string, any>;
	return { frontmatter, body: match[2] };
}

async function dirExists(path: string): Promise<boolean> {
	try {
		const s = await stat(path);
		return s.isDirectory();
	} catch {
		return false;
	}
}

export async function discoverSkills(agentDir: string): Promise<SkillMetadata[]> {
	const skillsDir = join(agentDir, "skills");
	if (!(await dirExists(skillsDir))) {
		return [];
	}

	const entries = await readdir(skillsDir, { withFileTypes: true });
	const skills: SkillMetadata[] = [];

	for (const entry of entries) {
		// Accept both real directories and symlinks pointing to directories
		if (!entry.isDirectory() && !entry.isSymbolicLink()) continue;

		const skillDir = join(skillsDir, entry.name);

		// For symlinks, verify the target is actually a directory
		if (entry.isSymbolicLink() && !(await dirExists(skillDir))) continue;
		const skillFile = join(skillDir, "SKILL.md");

		let content: string;
		try {
			content = await readFile(skillFile, "utf-8");
		} catch {
			continue; // no SKILL.md, skip
		}

		const { frontmatter } = parseFrontmatter(content);
		const name = frontmatter.name as string | undefined;
		const description = frontmatter.description as string | undefined;

		if (!name || !description) {
			console.warn(`Skipping skill "${entry.name}": missing name or description in frontmatter`);
			continue;
		}

		if (name !== entry.name) {
			console.warn(`Skipping skill "${entry.name}": name "${name}" does not match directory`);
			continue;
		}

		if (!KEBAB_RE.test(name)) {
			console.warn(`Skipping skill "${entry.name}": name must be kebab-case`);
			continue;
		}

		const meta: SkillMetadata = {
			name,
			description,
			directory: skillDir,
			filePath: skillFile,
		};

		// Parse optional learning fields
		if (typeof frontmatter.confidence === "number") meta.confidence = frontmatter.confidence;
		if (typeof frontmatter.usage_count === "number") meta.usage_count = frontmatter.usage_count;
		if (typeof frontmatter.success_count === "number") meta.success_count = frontmatter.success_count;
		if (typeof frontmatter.failure_count === "number") meta.failure_count = frontmatter.failure_count;
		if (typeof frontmatter.model === "string") meta.model = frontmatter.model;

		skills.push(meta);
	}

	return skills.sort((a, b) => a.name.localeCompare(b.name));
}

export async function loadSkill(meta: SkillMetadata): Promise<ParsedSkill> {
	const content = await readFile(meta.filePath, "utf-8");
	const { body } = parseFrontmatter(content);

	return {
		...meta,
		instructions: body.trim(),
		hasScripts: await dirExists(join(meta.directory, "scripts")),
		hasReferences: await dirExists(join(meta.directory, "references")),
	};
}

export function formatSkillsForPrompt(skills: SkillMetadata[]): string {
	if (skills.length === 0) return "";

	const skillEntries = skills
		.map((s) => {
			let entry = `<skill>\n<name>${s.name}</name>\n<description>${s.description}</description>`;
			entry += `\n<location>skills/${s.name}/SKILL.md</location>`;
			if (s.confidence !== undefined) {
				entry += `\n<confidence>${s.confidence}</confidence>`;
			}
			entry += "\n</skill>";
			return entry;
		})
		.join("\n");

	return `# Skills — FIRST PRIORITY (MANDATORY)

CRITICAL: You have installed skills that provide specialized capabilities.
Before attempting ANY task — simple or complex — you MUST check if an installed skill handles it.

## Rules (MUST follow in order)
1. ALWAYS scan the skill list below BEFORE taking ANY action on a user request
2. If a skill's description matches or partially matches the task, you MUST load its full
   instructions using the \`read\` tool: \`skills/<name>/SKILL.md\` — do this BEFORE anything else
3. Follow the loaded skill instructions EXACTLY — do NOT improvise or use alternative approaches
4. NEVER use general-purpose workarounds when a skill provides the right tool
   (e.g., use \`agent-browser open <url>\` NOT \`open -a Safari\`)
5. If multiple skills could apply, load the most specific one first
6. Even for seemingly simple tasks, CHECK SKILLS FIRST — skills often handle edge cases
   and produce higher quality results than ad-hoc approaches

## Enforcement
- If you skip checking skills and use a raw approach for a task that a skill handles,
  this is considered a FAILURE. Always check skills first.
- When calling \`task_tracker\` "begin", if it returns matching skills, you MUST load
  the top match immediately before proceeding.

<available_skills>
${skillEntries}
</available_skills>

To load a skill's full instructions: read \`skills/<name>/SKILL.md\`
Scripts within a skill are relative to the skill's directory: \`skills/<name>/scripts/\``;
}

export async function refreshSkills(agentDir: string): Promise<SkillMetadata[]> {
	return discoverSkills(agentDir);
}

export async function expandSkillCommand(
	input: string,
	skills: SkillMetadata[],
): Promise<{ expanded: string; skillName: string } | null> {
	const match = input.match(/^\/skill:([a-z0-9-]+)\s*([\s\S]*)$/);
	if (!match) return null;

	const skillName = match[1];
	const args = match[2].trim();

	const skill = skills.find((s) => s.name === skillName);
	if (!skill) return null;

	const parsed = await loadSkill(skill);

	let expanded = `<skill name="${skillName}" baseDir="${skill.directory}">
References are relative to ${skill.directory}.

${parsed.instructions}
</skill>
You MUST follow the skill instructions above. Do NOT use general alternatives.`;
	if (args) {
		expanded += `\n\n${args}`;
	}

	return { expanded, skillName };
}
