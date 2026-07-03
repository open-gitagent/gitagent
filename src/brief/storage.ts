import { readFile, writeFile, mkdir, readdir, access } from "fs/promises";
import { join } from "path";
import { createHash } from "crypto";
import type { Brief, BriefDraft, BriefStatus } from "./types.js";
import { BriefError } from "./types.js";

const BRIEFS_DIR = ".gitagent/briefs";

export function assertBriefApproved(brief: Brief): void {
	if (brief.status !== "approved") {
		throw new BriefError(
			`Brief "${brief.id}" (v${brief.version}) is not approved (status: "${brief.status}"). ` +
			"Refusing to run against an unvalidated or superseded brief.",
		);
	}
}

// Deterministic kebab-case slug from task string, max 60 chars
export function briefId(task: string): string {
	const slug = task
		.toLowerCase()
		.replace(/[^a-z0-9\s-]/g, "")
		.trim()
		.replace(/\s+/g, "-")
		.replace(/-+/g, "-")
		.slice(0, 60)
		.replace(/-$/, "");
	return slug || "brief";
}

export function hashContent(content: string): string {
	return createHash("sha256").update(content).digest("hex").slice(0, 8);
}

function briefsDir(agentDir: string): string {
	return join(agentDir, BRIEFS_DIR);
}

function briefFilePath(agentDir: string, id: string, version: number): string {
	const suffix = version > 1 ? `-v${version}` : "";
	return join(briefsDir(agentDir), `${id}${suffix}.md`);
}

function serializeBrief(brief: Brief): string {
	const fm = [
		"---",
		`id: ${brief.id}`,
		`task: ${JSON.stringify(brief.task)}`,
		`agent: ${brief.agent}`,
		`created_at: ${brief.created_at}`,
		brief.approved_at ? `approved_at: ${brief.approved_at}` : null,
		`status: ${brief.status}`,
		`version: ${brief.version}`,
		`planner_model: ${brief.planner_model}`,
		`evaluator_model: ${brief.evaluator_model}`,
		`negotiation_iterations: ${brief.negotiation_iterations}`,
		`soul_hash: ${brief.soul_hash}`,
		`rules_hash: ${brief.rules_hash}`,
		`assertion_count: ${brief.draft.assertions.length}`,
		"---",
	].filter(Boolean).join("\n");

	const draft = brief.draft;
	const assertionTable = [
		"| # | Category | Assertion | How to Verify |",
		"|---|---|---|---|",
		...draft.assertions.map(a =>
			`| ${a.id} | ${a.category} | ${a.assertion} | ${a.test} |`,
		),
	].join("\n");

	const ambigSection = draft.ambiguities.length > 0
		? `## Ambiguities Flagged\n\n${draft.ambiguities.map(a => `> ⚠ "${a}"`).join("\n")}\n\n`
		: "";

	const constraintsSection = draft.constraints_applied.length > 0
		? `## Agent Constraints Applied\n\n${draft.constraints_applied.map(c => `- ${c}`).join("\n")}\n`
		: "";

	const body = [
		`# Brief: ${draft.task_summary}`,
		"",
		"## Task",
		"",
		draft.task_summary,
		"",
		ambigSection,
		"## Success Criteria",
		"",
		assertionTable,
		"",
		"## Quality Rubric",
		"",
		`- **Craft:** ${draft.rubric.craft}`,
		`- **Originality:** ${draft.rubric.originality}`,
		`- **Tone:** ${draft.rubric.tone}`,
		`- **Completeness:** ${draft.rubric.completeness}`,
		"",
		constraintsSection,
		`<!-- draft_json_start\n${JSON.stringify(draft)}\ndraft_json_end -->`,
	].join("\n");

	return `${fm}\n\n${body}`;
}

function parseBrief(content: string, filePath: string): Brief {
	// Extract YAML frontmatter
	const fmMatch = content.match(/^---\n([\s\S]*?)\n---/);
	if (!fmMatch) throw new Error(`Invalid brief file: missing frontmatter in ${filePath}`);

	const fm = fmMatch[1];
	const get = (key: string): string => {
		const m = fm.match(new RegExp(`^${key}:\\s*(.+)$`, "m"));
		return m ? m[1].trim() : "";
	};

	// Extract draft JSON from HTML comment
	const draftMatch = content.match(/<!-- draft_json_start\n([\s\S]*?)\ndraft_json_end -->/);
	if (!draftMatch) throw new Error(`Invalid brief file: missing draft_json in ${filePath}`);
	const draft = JSON.parse(draftMatch[1]) as BriefDraft;

	const taskRaw = get("task");
	const task = taskRaw.startsWith('"') ? JSON.parse(taskRaw) : taskRaw;
	const approvedAt = get("approved_at");

	return {
		id: get("id"),
		task,
		agent: get("agent"),
		created_at: get("created_at"),
		approved_at: approvedAt || undefined,
		status: get("status") as BriefStatus,
		version: parseInt(get("version"), 10) || 1,
		planner_model: get("planner_model"),
		evaluator_model: get("evaluator_model"),
		negotiation_iterations: parseInt(get("negotiation_iterations"), 10) || 1,
		soul_hash: get("soul_hash"),
		rules_hash: get("rules_hash"),
		draft,
		file_path: filePath,
	};
}

export async function saveBrief(agentDir: string, brief: Brief): Promise<string> {
	await mkdir(briefsDir(agentDir), { recursive: true });
	const filePath = briefFilePath(agentDir, brief.id, brief.version);
	brief.file_path = filePath;
	await writeFile(filePath, serializeBrief(brief), "utf-8");
	return filePath;
}

export async function loadBriefFromFile(filePath: string): Promise<Brief> {
	const content = await readFile(filePath, "utf-8");
	return parseBrief(content, filePath);
}

// Accepts a full path OR just a brief name/id.
// "write-a-500-word-blog-post" → "<agentDir>/.gitagent/briefs/write-a-500-word-blog-post.md"
export function resolveBriefPath(agentDir: string, nameOrPath: string): string {
	if (nameOrPath.includes("/") || nameOrPath.includes("\\") || nameOrPath.endsWith(".md")) {
		return nameOrPath; // already a path
	}
	return join(briefsDir(agentDir), `${nameOrPath}.md`);
}

export async function findBrief(agentDir: string, task: string): Promise<Brief | null> {
	const id = briefId(task);
	const dir = briefsDir(agentDir);
	try {
		await access(dir);
	} catch {
		return null;
	}

	const files = await readdir(dir);
	const versionOf = (filename: string): number => {
		if (filename === `${id}.md`) return 1;
		const m = filename.match(/-v(\d+)\.md$/);
		return m ? parseInt(m[1], 10) : 1;
	};
	// Find all files matching the id, sorted by version number descending (numeric, not lexicographic)
	const matching = files
		.filter(f => f === `${id}.md` || f.match(new RegExp(`^${id}-v\\d+\\.md$`)))
		.sort((a, b) => versionOf(b) - versionOf(a));

	for (const filename of matching) {
		try {
			const brief = await loadBriefFromFile(join(dir, filename));
			if (brief.status === "approved") return brief;
		} catch {
			// skip malformed files
		}
	}
	return null;
}

export async function listBriefs(agentDir: string): Promise<Brief[]> {
	const dir = briefsDir(agentDir);
	try {
		await access(dir);
	} catch {
		return [];
	}

	const files = await readdir(dir);
	const briefs: Brief[] = [];
	for (const filename of files.filter(f => f.endsWith(".md"))) {
		try {
			briefs.push(await loadBriefFromFile(join(dir, filename)));
		} catch {
			// skip malformed
		}
	}
	return briefs.sort((a, b) => b.created_at.localeCompare(a.created_at));
}

export async function isBriefStale(
	agentDir: string,
	brief: Brief,
): Promise<{ stale: boolean; reason?: string }> {
	const reasons: string[] = [];

	const readOrEmpty = async (p: string) => {
		try { return await readFile(join(agentDir, p), "utf-8"); } catch { return ""; }
	};

	const soul = await readOrEmpty("SOUL.md");
	const rules = await readOrEmpty("RULES.md");

	const currentSoulHash = hashContent(soul);
	const currentRulesHash = hashContent(rules);

	if (brief.soul_hash && currentSoulHash !== brief.soul_hash) {
		reasons.push("Agent identity (SOUL.md) has changed since this brief was created. Tone assertions may be outdated.");
	}
	if (brief.rules_hash && currentRulesHash !== brief.rules_hash) {
		reasons.push("Agent rules (RULES.md) have changed since this brief was created. Constraint assertions may be outdated.");
	}

	return reasons.length > 0 ? { stale: true, reason: reasons.join(" ") } : { stale: false };
}

export async function archiveBrief(agentDir: string, briefId: string): Promise<void> {
	const dir = briefsDir(agentDir);
	const files = await readdir(dir).catch(() => [] as string[]);
	const matching = files.filter(f => f === `${briefId}.md` || f.match(new RegExp(`^${briefId}-v\\d+\\.md$`)));

	for (const filename of matching) {
		const filePath = join(dir, filename);
		try {
			const brief = await loadBriefFromFile(filePath);
			if (brief.status !== "archived") {
				brief.status = "archived";
				await writeFile(filePath, serializeBrief(brief), "utf-8");
			}
		} catch {
			// skip malformed
		}
	}
}

export async function nextVersion(agentDir: string, id: string): Promise<number> {
	const dir = briefsDir(agentDir);
	const files = await readdir(dir).catch(() => [] as string[]);
	const versions = files
		.map(f => {
			if (f === `${id}.md`) return 1;
			const m = f.match(new RegExp(`^${id}-v(\\d+)\\.md$`));
			return m ? parseInt(m[1], 10) : 0;
		})
		.filter(v => v > 0);
	return versions.length > 0 ? Math.max(...versions) + 1 : 1;
}
