import { readFile } from "fs/promises";
import { join } from "path";
import type { Brief, BriefIssue, IssueLevel } from "./types.js";
import { isBriefStale } from "./storage.js";
import { runBriefEvaluator, buildEvaluatorInput } from "./evaluator.js";

export interface AffectedAssertion {
	assertion_id: number | null;
	category: string;
	assertion_text: string;
	level: IssueLevel;
	issue: string;
	fix: string;
}

export interface StalenessReport {
	stale: boolean;
	soulChanged: boolean;
	rulesChanged: boolean;
	affectedAssertions: AffectedAssertion[];
	summary: string;
}

async function readOrEmpty(agentDir: string, filename: string): Promise<string> {
	try {
		return await readFile(join(agentDir, filename), "utf-8");
	} catch {
		return "";
	}
}

export async function analyzeStaleAssertions(
	agentDir: string,
	brief: Brief,
): Promise<StalenessReport> {
	const basicStale = await isBriefStale(agentDir, brief);

	if (!basicStale.stale) {
		return {
			stale: false,
			soulChanged: false,
			rulesChanged: false,
			affectedAssertions: [],
			summary: "",
		};
	}

	const soulChanged  = basicStale.reason?.includes("SOUL.md") ?? false;
	const rulesChanged = basicStale.reason?.includes("RULES.md") ?? false;

	// Read current files to pass to evaluator
	const soul   = await readOrEmpty(agentDir, "SOUL.md");
	const rules  = await readOrEmpty(agentDir, "RULES.md");
	const duties = await readOrEmpty(agentDir, "DUTIES.md");

	// Run the Evaluator on the existing draft with the NEW identity files.
	// It will naturally flag assertions that now conflict with or are missing from SOUL/RULES.
	let affected: AffectedAssertion[] = [];
	let summary = basicStale.reason ?? "Agent identity has changed.";

	try {
		const { verdict } = await runBriefEvaluator({
			input: buildEvaluatorInput(brief.draft, brief.task, soul, rules, duties, 1),
			agentDir,
		});

		// Only surface critical and warning issues — suggestions are noise during stale review
		const relevantIssues = verdict.issues.filter(
			(i: BriefIssue) => i.level === "critical" || i.level === "warning",
		);

		if (relevantIssues.length === 0) {
			summary = "Brief may be outdated but no specific assertions were flagged. You can use it as-is or regenerate.";
		} else {
			summary = verdict.summary;
		}

		// Map issues to AffectedAssertion display objects
		affected = relevantIssues.map((issue: BriefIssue) => {
			const matchedAssertion = issue.assertion_id != null
				? brief.draft.assertions.find(a => a.id === issue.assertion_id)
				: null;

			return {
				assertion_id: issue.assertion_id ?? null,
				category: matchedAssertion?.category ?? "overall",
				assertion_text: matchedAssertion?.assertion ?? "(overall brief structure)",
				level: issue.level,
				issue: issue.issue,
				fix: issue.fix,
			};
		});
	} catch {
		// Evaluator failure during stale analysis is non-fatal — fall back to vague warning
		summary = basicStale.reason ?? "Agent identity has changed. Consider regenerating the brief.";
	}

	return {
		stale: true,
		soulChanged,
		rulesChanged,
		affectedAssertions: affected,
		summary,
	};
}
