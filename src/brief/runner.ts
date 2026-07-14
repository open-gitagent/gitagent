import { query } from "../sdk.js";
import type { GCMessage } from "../sdk-types.js";
import type { Brief, AssertionResult, OutputVerdict, BriefOptions, RunWithBriefOptions } from "./types.js";
import { BriefError } from "./types.js";
import { loadBriefFromFile, resolveBriefPath, assertBriefApproved } from "./storage.js";
import { buildBriefSuffix } from "./injector.js";
import { runBriefOrchestration } from "./orchestrator.js";
import { runOutputEvaluator } from "./output-evaluator.js";
import { displayOutputReport } from "./report.js";
import { CostTracker } from "../cost-tracker.js";
import type { SessionCosts } from "../cost-tracker.js";
import { loadAgent } from "../loader.js";

const isTTY = Boolean(process.stdout.isTTY);
const dim = (s: string) => isTTY ? `\x1b[2m${s}\x1b[0m` : s;

async function resolveBrief(agentDir: string, briefOpt: BriefOptions, task: string, model?: string): Promise<Brief> {
	if (briefOpt.briefPath) {
		const brief = await loadBriefFromFile(resolveBriefPath(agentDir, briefOpt.briefPath));
		assertBriefApproved(brief);
		return brief;
	}
	if (briefOpt.autoBrief !== false) {
		const result = await runBriefOrchestration({ task, agentDir, model, options: briefOpt });
		if (result.skipped) {
			throw new BriefError("Brief was not approved — cannot run with an unapproved brief.");
		}
		return result.brief;
	}
	throw new BriefError(
		"runWithBrief() requires brief.briefPath or brief.autoBrief. " +
		"Create a brief first with runBriefOrchestration(), or pass autoBrief: true.",
	);
}

function buildRetryPrompt(originalPrompt: string, failures: AssertionResult[], previousOutput: string): string {
	const truncatedPrompt = originalPrompt.length > 1000
		? originalPrompt.slice(0, 1000) + "\n[...prompt truncated for retry...]"
		: originalPrompt;

	const failureList = failures.map(f =>
		`- [${f.category}] Assertion ${f.assertion_id}: "${f.assertion}"\n  Evidence of failure: ${f.evidence}`,
	).join("\n\n");

	return `${truncatedPrompt}

---
PREVIOUS RESPONSE (revise this — keep everything that already passed unchanged):
${previousOutput}

---
Your previous response did not satisfy these success criteria. Revise the response above
to address each failure below. Do not change parts that already passed.

Failed assertions:
${failureList}

Produce a complete revised response that satisfies all criteria.`;
}

export async function* runWithBrief(opts: RunWithBriefOptions): AsyncGenerator<GCMessage> {
	const {
		prompt,
		dir,
		model,
		briefModel,
		brief: briefOpt,
		maxRetries = 2,
		autoRetry = true,
		showReport = true,
		env,
		hooks,
		abortController,
		sessionId,
	} = opts;

	const agentDir = dir ?? process.cwd();

	// Resolve brief model: explicit opt > manifest brief.model > agent model
	let resolvedBriefModel = briefModel;
	if (!resolvedBriefModel) {
		try {
			const loaded = await loadAgent(agentDir);
			resolvedBriefModel = loaded.manifest.brief?.model ?? model;
		} catch {
			resolvedBriefModel = model;
		}
	}

	const brief = await resolveBrief(agentDir, briefOpt, prompt, resolvedBriefModel);

	if (brief.draft.assertions.length === 0) {
		console.log(dim("[brief] No assertions in brief — skipping output evaluation."));
		yield* query({ prompt, dir, model, env, hooks, abortController, sessionId });
		return;
	}

	const briefSuffix = buildBriefSuffix(brief);
	const costTracker = new CostTracker();

	let lastOutputText = "";
	let lastVerdict: OutputVerdict | null = null;
	let attempt = 0;
	let currentPrompt = prompt;

	while (attempt <= maxRetries) {
		attempt++;
		const outputMessages: string[] = [];

		const gen = query({
			prompt: currentPrompt,
			dir,
			model,
			env,
			systemPromptSuffix: briefSuffix,
			hooks,
			abortController,
			sessionId,
		});

		for await (const msg of gen) {
			yield msg;
			if (msg.type === "assistant") {
				outputMessages.push(msg.content);
			}
		}

		// Cap to last 5 messages and 12000 chars to keep evaluator input manageable.
		// The evaluator needs token budget to produce its JSON verdict.
		const cappedMessages = outputMessages.slice(-5);
		const labeled = cappedMessages
			.map((m, i) => `[Message ${outputMessages.length - cappedMessages.length + i + 1}]:\n${m}`)
			.join("\n\n---\n\n");
		lastOutputText = labeled.length > 12000
			? labeled.slice(-12000)
			: labeled;

		for (const [m, u] of Object.entries(gen.costs().modelUsage)) {
			costTracker.add(m, u);
		}

		const evalResult = await runOutputEvaluator({
			outputText: lastOutputText,
			brief,
			dir: agentDir,
			model: resolvedBriefModel,
		});
		lastVerdict = evalResult.verdict;
		for (const [m, u] of Object.entries(evalResult.costs.modelUsage)) {
			costTracker.add(m, u);
		}

		if (lastVerdict.all_passed || !autoRetry || attempt > maxRetries) break;

		const failures = lastVerdict.results.filter(r => !r.passed);
		currentPrompt = buildRetryPrompt(prompt, failures, lastOutputText);

		const failureDetail = failures
			.map(f => `  ✗ [${f.category}] Assertion ${f.assertion_id}: ${f.assertion}\n    Evidence: ${f.evidence}`)
			.join("\n");

		yield {
			type: "system",
			subtype: "session_start",
			content: `[brief] Retry ${attempt}/${maxRetries} — ${failures.length} assertion${failures.length !== 1 ? "s" : ""} failed\n${failureDetail}`,
			metadata: { briefRetry: true, attempt, failedCount: failures.length, failures },
		} satisfies GCMessage;
	}

	if (lastVerdict) {
		if (showReport) {
			displayOutputReport(lastVerdict, attempt);
		}

		yield {
			type: "system",
			subtype: "session_end",
			content: JSON.stringify(lastVerdict),
			metadata: { briefReport: true, attempts: attempt, costs: costTracker.get() },
		} satisfies GCMessage;
	}
}
