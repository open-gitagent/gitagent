import { query } from "../sdk.js";
import type { Brief, OutputVerdict } from "./types.js";
import type { GCAssistantMessage } from "../sdk-types.js";
import { CostTracker } from "../cost-tracker.js";
import type { SessionCosts } from "../cost-tracker.js";

const OUTPUT_EVALUATOR_SYSTEM_PROMPT = `You are an Output Quality Reviewer. You evaluate whether an AI agent's response
satisfied a set of pre-approved success criteria (assertions).

You will receive:
- OUTPUT: the agent's full conversation output, split into labeled messages ([Message 1], [Message 2], etc.)
- ASSERTIONS: a list of success criteria, each with an id, category, assertion text,
  and test instruction describing exactly how to verify it

IMPORTANT — IDENTIFY THE DELIVERABLE FIRST:
The agent may have sent multiple messages including thinking steps, verification checks,
word count notes, grep results, self-evaluation tables, and meta-commentary.
Your first task is to identify which message(s) contain the actual deliverable
(the final content the agent was asked to produce). Ignore all messages that are
verification steps, meta-commentary, or process notes. Evaluate ONLY the deliverable content.

If multiple messages contain versions of the deliverable, use the LAST complete version.

Your job is to evaluate EACH assertion independently against the deliverable.
Be objective and evidence-based. Quote directly from the deliverable when citing evidence.

Your output MUST be a single valid JSON object. No markdown. No code blocks. No explanation.
Raw JSON only.

Required JSON schema — fields are ordered deliberately: work out and write "evidence" and
"notes" BEFORE deciding "passed". Reason first, then commit to the verdict based on what
you just wrote. Never decide "passed" before "evidence" — a verdict written before its
reasoning is exactly the mistake this ordering exists to prevent.
{
  "all_passed": boolean,
  "passed_count": number,
  "failed_count": number,
  "results": [
    {
      "assertion_id": number,
      "category": string,
      "assertion": string,
      "evidence": string,
      "notes": string,
      "passed": boolean
    }
  ],
  "summary": string
}

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
EVALUATION RULES
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

1. FOLLOW THE TEST INSTRUCTION. Each assertion has a "test" field describing exactly
   how to verify it. Follow that instruction precisely.
   If test says "count words — must be 800-1000": count the words in the output.
   If test says "grep for competitor names — must return 0 matches": scan for those names.

2. BINARY ONLY. Each assertion is either passed or failed. No partial credit.
   If word count is 1050 and limit is 1000: FAIL.
   If word count is 999: PASS.

3. EVIDENCE IS REQUIRED. The evidence field must quote or cite something specific
   from the deliverable — not just restate the assertion.
   BAD:  "The output does not have 3 arguments"
   GOOD: "Found only 2 arguments: [X] and [Y]. No third."
   Keep evidence under 100 characters. Be precise, not verbose.

4. DO NOT INFER INTENT. If an assertion says "ends with an imperative verb" and
   the deliverable ends with a declarative sentence: FAIL. Do not give credit for close.

5. FORMAT ASSERTIONS: For assertions involving word counts, structure, or length:
   measure precisely on the deliverable only. Do not count verification tables or meta-commentary.

6. CONSTRAINT ASSERTIONS: For "no X mentioned" — scan the deliverable only. If found: FAIL.
   Quote the exact line where the violation occurs in evidence.

7. TONE ASSERTIONS: These are harder to evaluate. Use the test field strictly.
   If the test says "grep for hedging phrases" — look for those exact phrases in the deliverable.
   If the test says "all sentences use active voice" — check each sentence in the deliverable.

8. BE STRICT. The point of the brief is to catch problems the agent missed.
   When in doubt: FAIL. A false positive (incorrect FAIL) is less harmful than
   a false negative (incorrect PASS) — the agent will simply revise.`;

export function buildOutputEvaluatorInput(outputText: string, brief: Brief): string {
	const assertionList = brief.draft.assertions.map(a =>
		`Assertion ${a.id} [${a.category}]:\n  Text: "${a.assertion}"\n  How to verify: ${a.test}`,
	).join("\n\n");

	return [
		`OUTPUT (evaluate this):\n${outputText || "(empty output)"}`,
		`ASSERTIONS (evaluate each one):\n${assertionList}`,
	].join("\n\n---\n\n");
}

async function collectAssistantText(gen: AsyncIterable<any>): Promise<{ text: string; truncated: boolean; error?: string }> {
	let text = "";
	let truncated = false;
	let error: string | undefined;
	for await (const msg of gen) {
		if (msg.type === "assistant") {
			const am = msg as GCAssistantMessage;
			text = am.content;
			truncated = am.stopReason === "length";
		} else if (msg.type === "system" && msg.subtype === "error") {
			error = msg.content;
		}
	}
	return { text, truncated, error };
}

function extractJson(raw: string): string {
	const fenced = raw.match(/```(?:json)?\s*([\s\S]*?)```/);
	if (fenced) return fenced[1].trim();
	const start = raw.indexOf("{");
	const end = raw.lastIndexOf("}");
	if (start !== -1 && end !== -1 && end > start) return raw.slice(start, end + 1);
	return raw.trim();
}

const isTTY = Boolean(process.stdout.isTTY);
const yellow = (s: string) => isTTY ? `\x1b[33m${s}\x1b[0m` : s;

function fallbackVerdict(brief: Brief, warning: string, costs: SessionCosts): { verdict: OutputVerdict; costs: SessionCosts } {
	console.warn(yellow(`[brief] Output Evaluator warning: ${warning} — evaluation skipped, marking as failed.`));
	return {
		verdict: {
			all_passed: false,
			passed_count: 0,
			failed_count: brief.draft.assertions.length,
			results: brief.draft.assertions.map(a => ({
				assertion_id: a.id,
				category: a.category,
				assertion: a.assertion,
				passed: false,
				evidence: "(evaluation unavailable — could not parse evaluator response)",
			})),
			summary: `Output evaluation failed: ${warning}`,
		},
		costs,
	};
}

export async function runOutputEvaluator(opts: {
	outputText: string;
	brief: Brief;
	dir: string;
	model?: string;
}): Promise<{ verdict: OutputVerdict; costs: SessionCosts }> {
	const { outputText, brief, dir, model } = opts;
	const costTracker = new CostTracker();

	if (brief.draft.assertions.length === 0) {
		return {
			verdict: {
				all_passed: true,
				passed_count: 0,
				failed_count: 0,
				results: [],
				summary: "No assertions to evaluate.",
			},
			costs: costTracker.get(),
		};
	}

	const input = buildOutputEvaluatorInput(outputText, brief);

	for (let attempt = 1; attempt <= 2; attempt++) {
		const prompt = attempt === 1
			? input
			: `${input}\n\n---\n\nIMPORTANT: Your previous response was not valid JSON. Output ONLY a raw JSON object matching the schema. No markdown, no code fences, no explanation.`;

		const gen = query({
			prompt,
			dir,
			model,
			systemPrompt: OUTPUT_EVALUATOR_SYSTEM_PROMPT,
			replaceBuiltinTools: true,
			maxTurns: 1,
			constraints: { temperature: 0, maxTokens: 8000 },
		});

		try {
			const { text: raw, truncated, error } = await collectAssistantText(gen);
			for (const [m, u] of Object.entries(gen.costs().modelUsage)) {
				costTracker.add(m, u);
			}
			if (error) {
				throw new Error(`evaluator LLM call failed: ${error}`);
			}
			if (truncated) {
				throw new Error("response was truncated (hit max_tokens before completing JSON)");
			}
			if (!raw.trim()) {
				throw new Error("evaluator returned an empty response");
			}
			const json = extractJson(raw);
			const parsed = JSON.parse(json) as OutputVerdict;

			if (!Array.isArray(parsed.results)) {
				throw new Error("Missing results array");
			}

			// Guard against the evaluator silently skipping assertions — treat any
			// assertion missing from the response as failed rather than ignoring it.
			const seenIds = new Set(parsed.results.map(r => r.assertion_id));
			const missing = brief.draft.assertions.filter(a => !seenIds.has(a.id));
			const results = [
				...parsed.results,
				...missing.map(a => ({
					assertion_id: a.id,
					category: a.category,
					assertion: a.assertion,
					passed: false,
					evidence: "(not evaluated — missing from evaluator response)",
				})),
			];

			// Recompute counts from actual results to guard against model arithmetic errors
			const passedCount = results.filter(r => r.passed).length;
			const failedCount = results.filter(r => !r.passed).length;
			return {
				verdict: {
					all_passed: failedCount === 0,
					passed_count: passedCount,
					failed_count: failedCount,
					results,
					summary: missing.length > 0
						? `${parsed.summary ?? ""} (${missing.length} assertion(s) were not evaluated by the model and were marked failed.)`.trim()
						: (parsed.summary ?? ""),
				},
				costs: costTracker.get(),
			};
		} catch (err: any) {
			for (const [m, u] of Object.entries(gen.costs().modelUsage)) {
				costTracker.add(m, u);
			}
			if (attempt === 2) {
				return fallbackVerdict(brief, `failed to parse evaluator response after 2 attempts: ${err.message}`, costTracker.get());
			}
		}
	}

	return fallbackVerdict(brief, "unexpected evaluator exit", costTracker.get());
}
