import { runPlanner, buildPlannerInput } from "./planner.js";
import { runBriefEvaluator, buildEvaluatorInput } from "./evaluator.js";
import type { BriefDraft, EvaluatorVerdict, NegotiatorOptions, NegotiationResult } from "./types.js";
import { CostTracker } from "../cost-tracker.js";

const isTTY  = Boolean(process.stdout.isTTY);
const dim    = (s: string) => isTTY ? `\x1b[2m${s}\x1b[0m` : s;
const green  = (s: string) => isTTY ? `\x1b[32m${s}\x1b[0m` : s;
const yellow = (s: string) => isTTY ? `\x1b[33m${s}\x1b[0m` : s;

const MAX_ITERATIONS = 3;

export async function negotiateBrief(opts: NegotiatorOptions): Promise<NegotiationResult> {
	const { task, soul, rules, duties, model, agentDir } = opts;

	let currentDraft: BriefDraft | null = null;
	let bestDraft: BriefDraft | null = null;
	let bestVerdict: EvaluatorVerdict | null = null;
	let bestScore = -1;
	let lastVerdict: EvaluatorVerdict | null = null;
	let iterations = 0;
	const costTracker = new CostTracker();

	while (iterations < MAX_ITERATIONS) {
		iterations++;

		const iterLabel = `  Iteration ${iterations}/${MAX_ITERATIONS}`;
		const isRevision = iterations > 1;

		process.stdout.write(dim(`${iterLabel} — Planner ${isRevision ? "revising" : "generating"} assertions...`));
		const plannerResult = await runPlanner({
			input: buildPlannerInput(task, soul, rules, duties, lastVerdict ?? undefined, currentDraft ?? undefined),
			model,
			agentDir,
		});
		currentDraft = plannerResult.draft;
		for (const [m, u] of Object.entries(plannerResult.costs.modelUsage)) costTracker.add(m, u);
		process.stdout.write("\r\x1b[K");

		process.stdout.write(dim(`${iterLabel} — Evaluator reviewing...`));
		const evaluatorResult = await runBriefEvaluator({
			input: buildEvaluatorInput(currentDraft, task, soul, rules, duties, iterations),
			model,
			agentDir,
		});
		lastVerdict = evaluatorResult.verdict;
		for (const [m, u] of Object.entries(evaluatorResult.costs.modelUsage)) costTracker.add(m, u);
		process.stdout.write("\r\x1b[K");

		const criticals = lastVerdict.issues.filter(i => i.level === "critical").length;
		const warnings  = lastVerdict.issues.filter(i => i.level === "warning").length;

		if (lastVerdict.approved) {
			console.log(dim(`${iterLabel} — score: ${lastVerdict.score}/100  `) + green("✓ approved"));
		} else {
			const issuesSummary = [
				criticals > 0 ? `${criticals} critical` : "",
				warnings  > 0 ? `${warnings} warning${warnings !== 1 ? "s" : ""}` : "",
			].filter(Boolean).join(", ");
			console.log(dim(`${iterLabel} — score: ${lastVerdict.score}/100`) + yellow(`  (${issuesSummary})`));
		}

		if (lastVerdict.score > bestScore) {
			bestScore = lastVerdict.score;
			bestDraft = currentDraft;
			bestVerdict = lastVerdict;
		}

		if (lastVerdict.approved) break;
	}

	const finalDraft = lastVerdict!.approved ? currentDraft! : (bestDraft ?? currentDraft!);
	const finalVerdict = lastVerdict!.approved ? lastVerdict! : (bestVerdict ?? lastVerdict!);

	return {
		draft: finalDraft,
		verdict: finalVerdict,
		iterations,
		bestEffort: !lastVerdict!.approved,
		costs: costTracker.get(),
	};
}
