import { readFile, writeFile } from "fs/promises";
import { join } from "path";
import yaml from "js-yaml";

// ── Types ───────────────────────────────────────────────────────────────

export interface SkillStats {
	confidence: number;        // 0.0–1.0
	usage_count: number;
	success_count: number;
	failure_count: number;
	negative_examples: string[]; // capped at 10
}

const MAX_NEGATIVE_EXAMPLES = 10;

const DEFAULT_STATS: SkillStats = {
	confidence: 1.0,
	usage_count: 0,
	success_count: 0,
	failure_count: 0,
	negative_examples: [],
};

// ── Confidence math ─────────────────────────────────────────────────────

export function adjustConfidence(
	current: SkillStats,
	outcome: "success" | "failure" | "partial",
	failureReason?: string,
): SkillStats {
	const stats = { ...current };
	stats.usage_count++;

	switch (outcome) {
		case "success":
			// Asymptotic to 1.0: conf + 0.1 * (1 - conf)
			stats.confidence = Math.min(1.0, stats.confidence + 0.1 * (1 - stats.confidence));
			stats.success_count++;
			break;
		case "failure":
			// 2x penalty (asymmetric loss)
			stats.confidence = Math.max(0.0, stats.confidence - 0.2);
			stats.failure_count++;
			if (failureReason) {
				stats.negative_examples = [
					...stats.negative_examples.slice(-(MAX_NEGATIVE_EXAMPLES - 1)),
					failureReason,
				];
			}
			break;
		case "partial":
			stats.confidence = Math.max(0.0, stats.confidence - 0.05);
			stats.failure_count++;
			if (failureReason) {
				stats.negative_examples = [
					...stats.negative_examples.slice(-(MAX_NEGATIVE_EXAMPLES - 1)),
					failureReason,
				];
			}
			break;
	}

	// Round to avoid floating-point drift
	stats.confidence = Math.round(stats.confidence * 100) / 100;

	return stats;
}

// ── SKILL.md frontmatter read/write ─────────────────────────────────────

function parseFrontmatter(content: string): { frontmatter: Record<string, any>; body: string } {
	const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
	if (!match) {
		return { frontmatter: {}, body: content };
	}
	const frontmatter = yaml.load(match[1]) as Record<string, any>;
	return { frontmatter, body: match[2] };
}

function serializeFrontmatter(frontmatter: Record<string, any>, body: string): string {
	const yamlStr = yaml.dump(frontmatter, { lineWidth: -1, noRefs: true }).trimEnd();
	return `---\n${yamlStr}\n---\n${body}`;
}

/**
 * Reads stats out of already-parsed frontmatter, filling defaults for missing or
 * malformed fields. Lets callers that have parsed SKILL.md for other reasons get
 * stats without a second read of the same file.
 */
export function statsFromFrontmatter(frontmatter: Record<string, any>): SkillStats {
	return {
		confidence: typeof frontmatter.confidence === "number" ? frontmatter.confidence : DEFAULT_STATS.confidence,
		usage_count: typeof frontmatter.usage_count === "number" ? frontmatter.usage_count : DEFAULT_STATS.usage_count,
		success_count: typeof frontmatter.success_count === "number" ? frontmatter.success_count : DEFAULT_STATS.success_count,
		failure_count: typeof frontmatter.failure_count === "number" ? frontmatter.failure_count : DEFAULT_STATS.failure_count,
		negative_examples: Array.isArray(frontmatter.negative_examples) ? frontmatter.negative_examples : [],
	};
}

export async function loadSkillStats(skillDir: string): Promise<SkillStats> {
	const skillFile = join(skillDir, "SKILL.md");
	try {
		const content = await readFile(skillFile, "utf-8");
		const { frontmatter } = parseFrontmatter(content);
		return statsFromFrontmatter(frontmatter);
	} catch {
		return { ...DEFAULT_STATS };
	}
}

export async function saveSkillStats(skillDir: string, stats: SkillStats): Promise<void> {
	const skillFile = join(skillDir, "SKILL.md");
	const content = await readFile(skillFile, "utf-8");
	const { frontmatter, body } = parseFrontmatter(content);

	frontmatter.confidence = stats.confidence;
	frontmatter.usage_count = stats.usage_count;
	frontmatter.success_count = stats.success_count;
	frontmatter.failure_count = stats.failure_count;
	frontmatter.negative_examples = stats.negative_examples;

	await writeFile(skillFile, serializeFrontmatter(frontmatter, body), "utf-8");
}

export function isSkillFlagged(stats: SkillStats): boolean {
	return stats.confidence < 0.4;
}
