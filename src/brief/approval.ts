import { createInterface } from "readline";
import type { Brief, EvaluatorVerdict, BriefIssue } from "./types.js";
import type { StalenessReport } from "./stale.js";
import { colorCategory } from "./colors.js";

// ANSI helpers
const isTTY  = Boolean(process.stdout.isTTY);
const dim    = (s: string) => isTTY ? `\x1b[2m${s}\x1b[0m` : s;
const bold   = (s: string) => isTTY ? `\x1b[1m${s}\x1b[0m` : s;
const yellow = (s: string) => isTTY ? `\x1b[33m${s}\x1b[0m` : s;
const cyan   = (s: string) => isTTY ? `\x1b[36m${s}\x1b[0m` : s;
const green  = (s: string) => isTTY ? `\x1b[32m${s}\x1b[0m` : s;
const red    = (s: string) => isTTY ? `\x1b[31m${s}\x1b[0m` : s;

export type ApprovalDecision = "approve" | "edit" | "regenerate" | "skip";
export type StaleDecision = "use" | "regenerate" | "skip";

export function displayBrief(brief: Brief, verdict: EvaluatorVerdict, bestEffort: boolean): void {
	const draft = brief.draft;
	const warnings = verdict.issues.filter(i => i.level === "warning");
	const criticals = verdict.issues.filter(i => i.level === "critical");

	console.log("");
	console.log(bold(`◆ Brief ready for: "${brief.task}"`));
	console.log(dim(`  Agent: ${brief.agent}  |  Assertions: ${draft.assertions.length}  |  Complexity: ${draft.estimated_complexity}`));
	console.log(dim(`  Negotiation: ${brief.negotiation_iterations} iteration${brief.negotiation_iterations !== 1 ? "s" : ""}  |  Evaluator score: ${verdict.score}/100`));

	if (bestEffort) {
		console.log(yellow("\n  ⚠  Brief could not be fully validated after 3 iterations. Review warnings before approving."));
	}

	// Success criteria table
	const maxAssertion = draft.assertions.reduce((m, a) => Math.max(m, a.assertion.length), 0);
	const colWidth = Math.min(Math.max(maxAssertion, 30), 70);

	console.log("");
	console.log(dim("  ┌─ Success Criteria " + "─".repeat(Math.max(0, colWidth - 18)) + "┐"));
	for (const a of draft.assertions) {
		const cat = colorCategory(a.category);
		const assertion = a.assertion.length > colWidth ? a.assertion.slice(0, colWidth - 1) + "…" : a.assertion.padEnd(colWidth);
		console.log(`  │  ${a.id.toString().padStart(2)}. [${cat.padEnd(9 + cat.length - a.category.length)}] ${assertion}  │`);
	}
	console.log(dim("  └" + "─".repeat(colWidth + 22) + "┘"));

	// Quality rubric
	console.log("");
	console.log(dim("  Quality Rubric:"));
	console.log(dim(`    Craft:        ${draft.rubric.craft.slice(0, 80)}${draft.rubric.craft.length > 80 ? "…" : ""}`));
	console.log(dim(`    Originality:  ${draft.rubric.originality.slice(0, 80)}${draft.rubric.originality.length > 80 ? "…" : ""}`));
	console.log(dim(`    Tone:         ${draft.rubric.tone.slice(0, 80)}${draft.rubric.tone.length > 80 ? "…" : ""}`));
	console.log(dim(`    Completeness: ${draft.rubric.completeness.slice(0, 80)}${draft.rubric.completeness.length > 80 ? "…" : ""}`));

	// Ambiguities
	if (draft.ambiguities.length > 0) {
		console.log("");
		console.log(yellow("  ⚠  Ambiguities (auto-resolved):"));
		for (const a of draft.ambiguities) {
			console.log(yellow(`     → ${a}`));
		}
	}

	// Evaluator warnings
	if (warnings.length > 0) {
		console.log("");
		console.log(yellow("  ⚠  Evaluator warnings (non-blocking):"));
		for (const w of warnings) {
			console.log(yellow(`     → ${w.issue}`));
		}
	}

	// Critical issues (only shown in best-effort mode since they shouldn't reach here otherwise)
	if (criticals.length > 0) {
		console.log("");
		console.log(red("  ✗  Critical issues remaining:"));
		for (const c of criticals) {
			console.log(red(`     → ${c.issue}`));
		}
	}

	console.log("");
}

function isInteractive(): boolean {
	return process.stdin.isTTY && process.stdout.isTTY;
}

export async function promptApproval(brief: Brief): Promise<ApprovalDecision> {
	if (!isInteractive()) {
		// Non-interactive: auto-approve with log
		console.log(dim(`[brief] Non-interactive mode — auto-approving brief for: "${brief.task}"`));
		return "approve";
	}

	const rl = createInterface({
		input: process.stdin,
		output: process.stdout,
	});

	return new Promise((resolve) => {
		function ask() {
			console.log(
				"  " + bold("[A]") + "pprove  " +
				bold("[E]") + "dit  " +
				bold("[R]") + "egenerate from scratch  " +
				bold("[N]") + "ot now",
			);
			rl.question("  → ", (answer) => {
				const choice = answer.trim().toLowerCase();
				if (choice === "a" || choice === "approve") {
					rl.close();
					console.log(green("  ✓ Brief approved."));
					resolve("approve");
				} else if (choice === "e" || choice === "edit") {
					rl.close();
					console.log(dim("  Opening brief in editor..."));
					resolve("edit");
				} else if (choice === "r" || choice === "regenerate") {
					rl.close();
					console.log(dim("  Regenerating brief from scratch..."));
					resolve("regenerate");
				} else if (choice === "n") {
					rl.close();
					console.log(dim("  Brief not applied. You can run `gitagent brief` again at any time."));
					resolve("skip");
				} else {
					console.log(yellow("  Please enter A, E, R, or N."));
					ask();
				}
			});
		}
		ask();
	});
}

export function displayBriefList(briefs: Brief[]): void {
	if (briefs.length === 0) {
		console.log(dim("No briefs found. Run `gitagent brief \"task\"` to create one."));
		return;
	}

	console.log(bold(`\n${briefs.length} brief${briefs.length !== 1 ? "s" : ""} found:\n`));
	for (const b of briefs) {
		const statusColor = b.status === "approved" ? green : b.status === "draft" ? yellow : dim;
		console.log(`  ${statusColor(`[${b.status}]`)} ${cyan(b.id)} — ${b.task.slice(0, 60)}${b.task.length > 60 ? "…" : ""}`);
		console.log(dim(`           ${b.draft.assertions.length} assertions  |  v${b.version}  |  ${b.created_at.slice(0, 10)}`));
	}
	console.log("");
}

export function displayBriefDetail(brief: Brief): void {
	const draft = brief.draft;
	console.log(bold(`\nBrief: ${brief.id}`));
	console.log(dim(`Task:    ${brief.task}`));
	console.log(dim(`Agent:   ${brief.agent}`));
	console.log(dim(`Status:  ${brief.status}`));
	console.log(dim(`Created: ${brief.created_at}`));
	if (brief.approved_at) console.log(dim(`Approved: ${brief.approved_at}`));
	console.log("");

	console.log(bold("Success Criteria:"));
	for (const a of draft.assertions) {
		console.log(`  ${a.id}. [${colorCategory(a.category)}] ${a.assertion}`);
		console.log(dim(`     → Verify: ${a.test}`));
	}

	console.log("");
	console.log(bold("Rubric:"));
	console.log(`  Craft:        ${draft.rubric.craft}`);
	console.log(`  Originality:  ${draft.rubric.originality}`);
	console.log(`  Tone:         ${draft.rubric.tone}`);
	console.log(`  Completeness: ${draft.rubric.completeness}`);

	if (draft.constraints_applied.length > 0) {
		console.log("");
		console.log(bold("Constraints:"));
		for (const c of draft.constraints_applied) {
			console.log(`  - ${c}`);
		}
	}

	if (draft.ambiguities.length > 0) {
		console.log("");
		console.log(yellow("Ambiguities:"));
		for (const a of draft.ambiguities) {
			console.log(yellow(`  ⚠ ${a}`));
		}
	}
	console.log("");
}

export function displayStalenessReport(report: StalenessReport): void {
	if (!report.stale) return;

	const changed: string[] = [];
	if (report.soulChanged)  changed.push("SOUL.md");
	if (report.rulesChanged) changed.push("RULES.md");

	console.log("");
	console.log(yellow(`⚠  Brief is stale — ${changed.join(" and ")} changed.`));

	if (report.affectedAssertions.length === 0) {
		console.log(dim(`   ${report.summary}`));
	} else {
		console.log(yellow("   Affected assertions:\n"));
		for (const a of report.affectedAssertions) {
			const marker = a.level === "critical" ? red("  ✗ [critical]") : yellow("  ⚠ [warning] ");
			const assertionPreview = a.assertion_text.length > 60
				? a.assertion_text.slice(0, 60) + "…"
				: a.assertion_text;
			if (a.assertion_id != null) {
				console.log(`${marker} Assertion ${a.assertion_id} [${a.category}]: "${assertionPreview}"`);
			} else {
				console.log(`${marker} ${a.issue}`);
			}
			console.log(dim(`        Issue: ${a.issue}`));
			console.log(dim(`        Fix:   ${a.fix}`));
			console.log("");
		}
	}
}

export async function promptStaleDecision(): Promise<StaleDecision> {
	if (!isInteractive()) {
		console.log(dim("[brief] Non-interactive mode — using existing brief despite staleness."));
		return "use";
	}

	const rl = createInterface({
		input: process.stdin,
		output: process.stdout,
	});

	return new Promise((resolve) => {
		function ask() {
			console.log(
				"  " + bold("[U]") + "se anyway  " +
				bold("[R]") + "egenerate brief  " +
				bold("[N]") + "ot now",
			);
			rl.question("  → ", (answer) => {
				const choice = answer.trim().toLowerCase();
				if (choice === "u" || choice === "use") {
					rl.close();
					console.log(dim("  Using existing brief."));
					resolve("use");
				} else if (choice === "r" || choice === "regenerate") {
					rl.close();
					console.log(dim("  Regenerating brief from scratch..."));
					resolve("regenerate");
				} else if (choice === "n") {
					rl.close();
					console.log(dim("  Skipping. Brief not applied."));
					resolve("skip");
				} else {
					console.log(yellow("  Please enter U, R, or N."));
					ask();
				}
			});
		}
		ask();
	});
}
