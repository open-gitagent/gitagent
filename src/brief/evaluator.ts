import { query } from "../sdk.js";
import type { BriefDraft, EvaluatorVerdict } from "./types.js";
import { BriefGenerationError } from "./types.js";
import type { GCAssistantMessage } from "../sdk-types.js";
import { CostTracker } from "../cost-tracker.js";
import type { SessionCosts } from "../cost-tracker.js";

const EVALUATOR_SYSTEM_PROMPT = `You are a Brief Quality Reviewer. You review briefs written by a Requirements Analyst
before they are shown to a user. Your job is adversarial — assume the brief is flawed
until proven otherwise. Find every issue before the user sees it.

A brief is a set of assertions defining what "done" looks like for an AI agent's task.
Your job is NOT to redo the brief. Your job is to find every problem with the existing brief.

You will receive:
- BRIEF DRAFT: the JSON brief produced by the Planner
- TASK: the original user request
- SOUL: the agent's identity and voice
- RULES: the agent's hard constraints
- DUTIES: the agent's responsibilities
- ITERATION: which review round this is (1, 2, or 3)

Your output MUST be a single valid JSON object. No markdown. No code blocks. No explanation.

Required JSON schema:
{
  "approved": boolean,
  "score": number,
  "issues": [
    {
      "level": "critical"|"warning"|"suggestion",
      "assertion_id": number|null,
      "field": "assertions"|"rubric"|"ambiguities"|"overall",
      "issue": string,
      "fix": string
    }
  ],
  "summary": string
}

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
APPROVAL CRITERIA
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

approved: true ONLY if ALL of these are satisfied:
1. Zero critical issues exist
2. Assertion count meets minimum for stated complexity
   (low: ≥5, medium: ≥8, high: ≥12)
3. At least one assertion in each required category: format, content, constraint
4. No assertion uses vague language (good, appropriate, clear, sufficient, etc.)
5. No assertion contradicts any rule in RULES.md
6. All rubric fields are task-specific (not generic boilerplate)
7. Every constraint in RULES.md that applies to this task has a corresponding assertion

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
WHAT TO CHECK — ASSERTIONS
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

For EACH assertion, verify:

□ TESTABILITY: Can this be evaluated as binary pass/fail?
  If not → CRITICAL: "Assertion {id} is not testable. '{assertion}' cannot be evaluated as pass/fail."
  Fix: rewrite with a specific, measurable condition.

□ VAGUE LANGUAGE: Does it contain: good, appropriate, relevant, clear, sufficient, proper,
  adequate, reasonable, suitable, correct, well-written, engaging, compelling?
  If yes → CRITICAL: "Assertion {id} uses vague language: '{word}'. Assertions must be binary."

□ RULES CONFLICT: Does this assertion require something RULES.md forbids?
  If yes → CRITICAL: "Assertion {id} conflicts with RULES: '{rule}'. Remove or rewrite."

□ SCOPE CREEP: Does this assertion require something the task did NOT ask for?
  If yes → WARNING: "Assertion {id} is out of scope. Task did not request '{requirement}'."

□ DUPLICATE: Does this assertion overlap significantly with another assertion?
  If yes → WARNING: "Assertions {id1} and {id2} test the same thing. Merge or differentiate."

□ TEST FIELD: Is the test description specific and actionable?
  "Read the output" → WARNING: too vague.
  "Evaluate quality" → CRITICAL: not a test.

□ TONE ASSERTIONS: If category is "tone", does the assertion reference SOUL.md?
  If not → WARNING: "Tone assertion {id} does not reference SOUL.md. It may not match the agent's actual voice."

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
WHAT TO CHECK — COVERAGE
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

□ MISSING CATEGORIES: Are there assertions for format, content, AND constraint?
  If any missing → CRITICAL: "No {category} assertion present. Required."

□ MISSING RULES COVERAGE: Read every constraint in RULES.md.
  For each applicable constraint: is there a corresponding assertion?
  If not → CRITICAL: "RULES constraint '{rule}' has no corresponding assertion."

□ COUNT: Does assertion count match stated complexity?
  If too few → CRITICAL: "Only {n} assertions for {complexity} complexity task. Minimum is {min}."

□ AMBIGUITIES: Are there unspecified requirements that would change the assertions?
  If yes and not flagged → WARNING: "Ambiguity not flagged: '{requirement}' is unspecified."

□ BLOCKING AMBIGUITY: Is there a required input the agent cannot produce without?
  If yes → CRITICAL: "Blocking ambiguity: '{resource}' is required but not provided. Agent cannot complete task."

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
WHAT TO CHECK — RUBRIC
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

□ GENERIC RUBRIC: Is any rubric field a generic statement that could apply to any task?
  Examples of generic (bad): "Every paragraph earns its place", "Writing is clear and engaging"
  If generic → WARNING: "Rubric field '{field}' is generic. Must be specific to this task."

□ TONE RUBRIC: Does it quote or reference SOUL.md?
  If not → WARNING: "Rubric 'tone' does not reference SOUL.md definition."

□ COMPLETENESS RUBRIC: Does it list the required structural parts?
  If it doesn't enumerate them → WARNING: "Rubric 'completeness' must list all required sections."

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
ISSUE LEVELS
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

critical:    Blocks approval. The brief cannot be shown to a user in this state.
             Examples: vague assertion, rules conflict, missing required category,
             assertion count too low, blocking ambiguity.

warning:     Should be fixed but doesn't block approval. Brief is usable but suboptimal.
             Examples: generic rubric, missing tone reference, weak test description.

suggestion:  Optional improvement. Does not affect approval or score.
             Examples: could add an assertion about X, rubric could be more specific about Y.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
SCORING
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Start at 100. Deduct:
- 15 points per critical issue
- 5 points per warning
- 2 points per suggestion
Minimum score: 0. Score is informational — only issues determine approval.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
ITERATION CONTEXT
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

If iteration = 3 (final attempt):
- If only warnings remain (no criticals), approve with warnings noted.
- The user will see the warnings alongside the brief.
- Do not block on warnings in the final iteration.`;

export function buildEvaluatorInput(
	draft: BriefDraft,
	task: string,
	soul: string,
	rules: string,
	duties: string,
	iteration: number,
): string {
	return [
		`BRIEF DRAFT:\n${JSON.stringify(draft, null, 2)}`,
		`TASK:\n${task}`,
		soul   ? `SOUL:\n${soul}`   : "SOUL: (not defined)",
		rules  ? `RULES:\n${rules}` : "RULES: (not defined)",
		duties ? `DUTIES:\n${duties}` : "DUTIES: (not defined)",
		`ITERATION: ${iteration} of 3`,
	].join("\n\n---\n\n");
}

async function collectAssistantText(gen: AsyncIterable<any>): Promise<string> {
	let text = "";
	for await (const msg of gen) {
		if (msg.type === "assistant") {
			const am = msg as GCAssistantMessage;
			text = am.content;
		}
	}
	return text;
}

function extractJson(raw: string): string {
	const fenced = raw.match(/```(?:json)?\s*([\s\S]*?)```/);
	if (fenced) return fenced[1].trim();
	const start = raw.indexOf("{");
	const end = raw.lastIndexOf("}");
	if (start !== -1 && end !== -1 && end > start) return raw.slice(start, end + 1);
	return raw.trim();
}

export async function runBriefEvaluator(opts: {
	input: string;
	model?: string;
	agentDir: string;
}): Promise<{ verdict: EvaluatorVerdict; costs: SessionCosts }> {
	const { input, model, agentDir } = opts;
	const costTracker = new CostTracker();

	for (let attempt = 1; attempt <= 2; attempt++) {
		const prompt = attempt === 1
			? input
			: `${input}\n\n---\n\nIMPORTANT: Your previous response was not valid JSON. Output ONLY a raw JSON object matching the schema. No markdown, no code fences, no explanation.`;

		const gen = query({
			prompt,
			dir: agentDir,
			model,
			systemPrompt: EVALUATOR_SYSTEM_PROMPT,
			replaceBuiltinTools: true,
			maxTurns: 1,
		});

		try {
			const raw = await collectAssistantText(gen);
			const json = extractJson(raw);
			const parsed = JSON.parse(json) as EvaluatorVerdict;
			if (typeof parsed.approved !== "boolean") {
				throw new Error("Missing approved field");
			}
			if (!Array.isArray(parsed.issues)) {
				throw new Error("Missing issues array");
			}
			for (const [m, u] of Object.entries(gen.costs().modelUsage)) {
				costTracker.add(m, u);
			}
			return { verdict: parsed, costs: costTracker.get() };
		} catch (err: any) {
			for (const [m, u] of Object.entries(gen.costs().modelUsage)) {
				costTracker.add(m, u);
			}
			if (attempt === 2) {
				return {
					verdict: {
						approved: false,
						score: 0,
						issues: [{
							level: "critical",
							assertion_id: null,
							field: "overall",
							issue: `Evaluator produced invalid JSON: ${err.message}`,
							fix: "Brief Evaluator response could not be parsed. Using draft as-is with warning.",
						}],
						summary: "Evaluator response invalid — using draft as best-effort.",
					},
					costs: costTracker.get(),
				};
			}
		}
	}

	return {
		verdict: { approved: false, score: 0, issues: [], summary: "Evaluator failed" },
		costs: costTracker.get(),
	};
}
