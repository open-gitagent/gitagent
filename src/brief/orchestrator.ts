import { readFile } from "fs/promises";
import { join, resolve } from "path";
import type { Brief, BriefOptions } from "./types.js";
import { BriefError, BriefGenerationError } from "./types.js";
import { CostTracker } from "../cost-tracker.js";
import type { SessionCosts } from "../cost-tracker.js";
import { negotiateBrief } from "./negotiator.js";
import {
	briefId,
	hashContent,
	saveBrief,
	findBrief,
	loadBriefFromFile,
	resolveBriefPath,
	archiveBrief,
	nextVersion,
	assertBriefApproved,
} from "./storage.js";
import {
	displayBrief,
	displayBriefList,
	displayBriefDetail,
	displayStalenessReport,
	promptApproval,
	promptStaleDecision,
	type ApprovalDecision,
} from "./approval.js";
import { buildBriefSuffix } from "./injector.js";
import { analyzeStaleAssertions } from "./stale.js";
import { openInEditor } from "./editor.js";

// ANSI helpers
const isTTY  = Boolean(process.stdout.isTTY);
const dim    = (s: string) => isTTY ? `\x1b[2m${s}\x1b[0m` : s;
const bold   = (s: string) => isTTY ? `\x1b[1m${s}\x1b[0m` : s;
const yellow = (s: string) => isTTY ? `\x1b[33m${s}\x1b[0m` : s;

async function readOrEmpty(agentDir: string, filename: string): Promise<string> {
	try {
		return await readFile(join(agentDir, filename), "utf-8");
	} catch {
		return "";
	}
}

async function loadParentRules(agentDir: string, extendsPath: string | undefined): Promise<string> {
	if (!extendsPath) return "";

	// Resolve: could be a relative path or a remote URL that was cloned into .gitagent/deps/
	let parentDir: string;

	if (extendsPath.startsWith("http") || extendsPath.startsWith("git@")) {
		// Remote URL — look in the cloned deps dir
		const parentName = extendsPath.split("/").pop()?.replace(/\.git$/, "") || "parent";
		parentDir = join(agentDir, ".gitagent", "deps", parentName);
	} else {
		// Local relative path
		parentDir = resolve(agentDir, extendsPath);
	}

	const parentRules = await readOrEmpty(parentDir, "RULES.md");
	return parentRules
		? `\n\n--- Inherited Rules (from ${extendsPath}) ---\n\n${parentRules}`
		: "";
}

export interface BriefOrchestrationOptions {
	task: string;
	agentDir: string;
	agentName?: string;
	agentExtends?: string;
	model?: string;
	options?: BriefOptions;
}

export interface BriefOrchestrationResult {
	brief: Brief;
	systemPromptSuffix: string;
	skipped: boolean;
	costs: SessionCosts;
}

export interface GenerateBriefResult {
	filePath: string;
	costs: SessionCosts;
}

export async function runBriefOrchestration(opts: BriefOrchestrationOptions): Promise<BriefOrchestrationResult> {
	const { task, agentDir, agentName = "agent", agentExtends, model, options = {} } = opts;

	if (!task || task.trim() === "") {
		throw new BriefError("Task cannot be empty.");
	}

	const effectiveTask = task.length > 2000
		? task.slice(0, 2000)
		: task;

	if (task.length > 2000) {
		console.log(yellow("[brief] Task truncated to 2000 chars for brief generation. Full task passed to main execution."));
	}

	const zeroCosts = () => new CostTracker().get();

	// If a specific brief path is given, load and use it directly
	if (options.briefPath) {
		const brief = await loadBriefFromFile(resolveBriefPath(agentDir, options.briefPath));
		assertBriefApproved(brief);
		const staleReport = await analyzeStaleAssertions(agentDir, brief);
		if (staleReport.stale) {
			displayStalenessReport(staleReport);
		}
		return {
			brief,
			systemPromptSuffix: buildBriefSuffix(brief),
			skipped: false,
			costs: zeroCosts(),
		};
	}

	// Check for existing approved brief unless regenerate is requested
	if (!options.regenerate) {
		const existing = await findBrief(agentDir, effectiveTask);
		if (existing) {
			const staleReport = await analyzeStaleAssertions(agentDir, existing);
			if (staleReport.stale) {
				displayStalenessReport(staleReport);
				const staleDecision = await promptStaleDecision();

				if (staleDecision === "skip") {
					return { brief: existing, systemPromptSuffix: "", skipped: true, costs: zeroCosts() };
				}

				if (staleDecision === "regenerate") {
					// Fall through to negotiation below (skip the early return)
				} else {
					// "use" — proceed with existing brief despite staleness
					console.log(dim(`[brief] Using existing brief: ${existing.id} (v${existing.version})`));
					return {
						brief: existing,
						systemPromptSuffix: buildBriefSuffix(existing),
						skipped: false,
						costs: zeroCosts(),
					};
				}
			} else {
				console.log(dim(`[brief] Using existing approved brief: ${existing.id} (v${existing.version})`));
				return {
					brief: existing,
					systemPromptSuffix: buildBriefSuffix(existing),
					skipped: false,
					costs: zeroCosts(),
				};
			}
		}
	}

	// Load agent identity files for Planner/Evaluator context
	const soul   = await readOrEmpty(agentDir, "SOUL.md");
	const rules  = await readOrEmpty(agentDir, "RULES.md");
	const duties = await readOrEmpty(agentDir, "DUTIES.md");

	// Feature 3: Load parent RULES.md if agent inherits from a parent
	const parentRulesSuffix = await loadParentRules(agentDir, agentExtends);
	const effectiveRules = rules + parentRulesSuffix;

	if (!soul)  console.log(dim("[brief] No SOUL.md found — Planner will generate generic assertions."));
	if (!rules) console.log(dim("[brief] No RULES.md found — no constraint assertions will be generated."));
	if (parentRulesSuffix) console.log(dim(`[brief] Parent rules loaded from: ${agentExtends}`));

	console.log(bold(`\n[brief] Negotiating brief for: "${effectiveTask.slice(0, 60)}${effectiveTask.length > 60 ? "…" : ""}"`));
	console.log(dim("[brief] Planner and Evaluator are negotiating internally...\n"));

	// Run the Planner↔Evaluator negotiation loop
	const negotiation = await negotiateBrief({
		task: effectiveTask,
		soul,
		rules: effectiveRules,
		duties,
		model,
		agentDir,
	});

	// Compute hashes for stale detection (only child RULES hash; parent can change independently)
	const soulHash  = hashContent(soul);
	const rulesHash = hashContent(rules);

	const id = briefId(effectiveTask);

	// Archive old version if regenerating
	if (options.regenerate) {
		await archiveBrief(agentDir, id);
	}

	const version = await nextVersion(agentDir, id);
	const now = new Date().toISOString();

	let finalBrief: Brief = {
		id,
		task: effectiveTask,
		agent: agentName,
		created_at: now,
		status: "draft",
		version,
		planner_model: model ?? "default",
		evaluator_model: model ?? "default",
		negotiation_iterations: negotiation.iterations,
		soul_hash: soulHash,
		rules_hash: rulesHash,
		draft: negotiation.draft,
		file_path: "",
	};

	// Auto-approve path (programmatic use or no TTY)
	if (options.skipApproval || !process.stdin.isTTY) {
		if (negotiation.bestEffort && !options.allowBestEffort) {
			const criticals = negotiation.verdict.issues.filter(i => i.level === "critical");
			throw new BriefGenerationError(
				`Brief negotiation did not reach approval after ${negotiation.iterations} iteration(s) ` +
				`(score ${negotiation.verdict.score}/100). ${criticals.length} critical issue(s) remain:\n` +
				criticals.map(c => `  - ${c.issue}`).join("\n") +
				`\nPass { allowBestEffort: true } to accept this brief anyway, or fix the issues above.`,
			);
		}
		finalBrief.status = "approved";
		finalBrief.approved_at = now;
		if (negotiation.bestEffort) {
			console.log(yellow(`[brief] ⚠ Auto-approved a best-effort brief (score ${negotiation.verdict.score}/100, never reached full approval).`));
		}
		const filePath = await saveBrief(agentDir, finalBrief);
		console.log(dim(`[brief] Brief auto-approved and saved: ${filePath}`));
		return {
			brief: finalBrief,
			systemPromptSuffix: buildBriefSuffix(finalBrief),
			skipped: false,
			costs: negotiation.costs,
		};
	}

	// Interactive approval loop
	displayBrief(finalBrief, negotiation.verdict, negotiation.bestEffort);

	let currentNegotiation = negotiation;
	let attempts = 0;

	while (attempts < 3) {
		const decision: ApprovalDecision = await promptApproval(finalBrief);

		if (decision === "approve") {
			finalBrief.status = "approved";
			finalBrief.approved_at = new Date().toISOString();
			const filePath = await saveBrief(agentDir, finalBrief);
			console.log(dim(`[brief] Brief saved: ${filePath}`));
			return {
				brief: finalBrief,
				systemPromptSuffix: buildBriefSuffix(finalBrief),
				skipped: false,
				costs: currentNegotiation.costs,
			};
		}

		if (decision === "skip") {
			await saveBrief(agentDir, finalBrief);
			return {
				brief: finalBrief,
				systemPromptSuffix: "",
				skipped: true,
				costs: currentNegotiation.costs,
			};
		}

		if (decision === "edit") {
			// Feature 2: open brief in $EDITOR, re-validate, re-display
			const edited = await openInEditor(finalBrief.draft);
			if (edited) {
				finalBrief = { ...finalBrief, draft: edited };
				console.log(dim("[brief] Brief updated from editor."));
			} else {
				console.log(dim("[brief] Editor changes discarded or invalid. Showing original brief."));
			}
			displayBrief(finalBrief, currentNegotiation.verdict, currentNegotiation.bestEffort);
			continue; // re-prompt without incrementing attempts
		}

		// Regenerate
		attempts++;
		console.log(dim(`[brief] Regenerating (attempt ${attempts + 1})...\n`));
		const renegotiation = await negotiateBrief({
			task: effectiveTask,
			soul,
			rules: effectiveRules,
			duties,
			model,
			agentDir,
		});

		currentNegotiation = renegotiation;
		finalBrief = {
			...finalBrief,
			version: version + attempts,
			draft: renegotiation.draft,
			negotiation_iterations: renegotiation.iterations,
			created_at: new Date().toISOString(),
		};
		displayBrief(finalBrief, renegotiation.verdict, renegotiation.bestEffort);
	}

	// Save as draft after exhausting regenerate attempts
	await saveBrief(agentDir, finalBrief);
	return {
		brief: finalBrief,
		systemPromptSuffix: "",
		skipped: true,
		costs: currentNegotiation.costs,
	};
}

export async function generateBrief(opts: {
	task: string;
	dir?: string;
	model?: string;
	skipApproval?: boolean;
}): Promise<GenerateBriefResult | null> {
	const agentDir = opts.dir ?? process.cwd();
	const result = await runBriefOrchestration({
		task: opts.task,
		agentDir,
		model: opts.model,
		options: { skipApproval: opts.skipApproval },
	});
	if (result.skipped) return null;
	return {
		filePath: result.brief.file_path,
		costs: result.costs,
	};
}

// ── List/view helpers re-exported for CLI ─────────────────────────────────

export { listBriefs, loadBriefFromFile } from "./storage.js";
export { displayBriefList, displayBriefDetail } from "./approval.js";
export { buildBriefSuffix } from "./injector.js";
