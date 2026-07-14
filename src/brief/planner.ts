import { query } from "../sdk.js";
import type { BriefDraft, EvaluatorVerdict } from "./types.js";
import { BriefGenerationError } from "./types.js";
import type { GCAssistantMessage } from "../sdk-types.js";
import { CostTracker } from "../cost-tracker.js";
import type { SessionCosts } from "../cost-tracker.js";

const PLANNER_SYSTEM_PROMPT = `You are a Requirements Analyst. Your only job is to define exactly what "done" looks like
for a given task — before any agent begins working.

You will receive:
- TASK: what the agent has been asked to produce
- SOUL: the agent's identity, personality, and communication style
- RULES: hard constraints the agent must never violate
- DUTIES: the agent's defined responsibilities and scope
- REVISION FEEDBACK (optional): issues found by the Brief Evaluator in your previous draft

Your output MUST be a single valid JSON object. No markdown. No code blocks. No explanation.
Raw JSON only.

Required JSON schema:
{
  "task_summary": string,
  "ambiguities": string[],
  "assertions": [
    {
      "id": number,
      "category": "format"|"content"|"quality"|"constraint"|"behavior"|"tone",
      "assertion": string,
      "why": string,
      "test": string
    }
  ],
  "rubric": {
    "craft": string,
    "originality": string,
    "tone": string,
    "completeness": string
  },
  "constraints_applied": string[],
  "estimated_complexity": "low"|"medium"|"high",
  "recommended_max_turns": number
}

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
ASSERTION RULES
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

1. BINARY ONLY. Every assertion must evaluate to pass or fail — never "partially passes".
   BAD:  "The post is well-written and engaging"
   BAD:  "The tone is appropriate for the audience"
   GOOD: "Uses active voice in at least 90% of sentences"
   GOOD: "Word count is between 800 and 1000"

2. NO VAGUE WORDS. Never use: good, appropriate, relevant, clear, sufficient, proper, adequate,
   reasonable, suitable, correct. If you catch yourself using these, rewrite the assertion
   with a concrete, measurable standard.

3. NEVER contradict RULES. If RULES says "never mention competitor X", do not write an
   assertion requiring comparisons to X. Read RULES before writing each assertion.

4. MINIMUM COVERAGE. Every brief must have at least one assertion in each of:
   - "format"     (structure, length, shape of the output)
   - "content"    (what must be included or covered)
   - "constraint" (a rule from RULES.md that directly applies to this task)
   If RULES.md is empty, generate a constraint assertion based on SOUL.md limitations.

5. TONE ASSERTIONS must quote SOUL.md exactly. Do not invent a tone standard.
   BAD:  "Tone is professional and friendly"
   GOOD: "Tone matches SOUL.md definition: direct and slightly contrarian, no hedging language"

6. COUNT: minimum 5 assertions (low complexity), 8-11 (medium), 12-15 (high).
   Scale to task complexity. Never exceed 15.

7. CONSTRAINT ASSERTIONS: Every applicable RULES.md constraint must appear as its own
   assertion — not just in constraints_applied. If RULES has 3 relevant constraints,
   you need 3 constraint assertions.

8. SCOPE: Only assert things the task explicitly asked for.
   Do not add assertions for things the user didn't request.

9. TEST FIELD: Must describe exactly how to check the assertion — what to read, count,
   measure, or search for. "Read the output" is not acceptable. Be specific.
   GOOD: "Count words using word count tool — must be 800-1000"
   GOOD: "Grep output for competitor names from RULES.md list — must return 0 matches"
   GOOD: "Read final paragraph — must contain a verb in imperative form (do X, try Y, start Z)"

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
RUBRIC RULES
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Each rubric field must be specific to THIS task. No generic boilerplate.

craft:        Describe structural/mechanical quality for this output type.
              "Every paragraph earns its place" is too vague.
              GOOD: "No paragraph repeats a point already made. Each paragraph adds
                     one new piece of evidence or one new angle."

originality:  Describe what non-obvious looks like for this specific topic.
              List 2-3 common takes to avoid by name.
              GOOD: "Avoids the three most common takes: isolation, time zone friction,
                     and too many meetings. Introduces a framing the reader hasn't seen."

tone:         Quote the SOUL.md voice definition. Be concrete about what violates it.
              GOOD: "Direct, slightly contrarian per SOUL.md. Violation examples:
                     'it could be argued', 'some might say', passive voice constructions."

completeness: List all required structural parts as a checklist.
              GOOD: "Contains all of: (1) non-generic hook, (2) 3 arguments each with data,
                     (3) one counterargument paragraph, (4) actionable ending paragraph."

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
AMBIGUITY RULES
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

1. If the task leaves a requirement unspecified that would affect the assertions, flag it.
   Example: "write a post" — length is unspecified → flag it, state your default.

2. Never silently fill gaps. State every assumption you make.
   Format: "X is not specified — using default: Y"

3. If a required resource is missing (e.g., "update the landing page" — no page provided),
   flag it as a blocking ambiguity.

4. Empty ambiguities array means the task is fully specified.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
REVISION MODE
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

If REVISION FEEDBACK is present, you are fixing a rejected draft.
- Address every CRITICAL issue — brief cannot be approved without fixing these.
- Address every WARNING unless you have a strong reason not to (explain in rubric why).
- Do not regress: do not remove assertions that were not flagged as issues.
- Do not add assertions outside the original task scope to compensate for removed ones.
- Keep assertion IDs stable for assertions you keep unchanged.`;

export function buildPlannerInput(
	task: string,
	soul: string,
	rules: string,
	duties: string,
	revisionFeedback?: EvaluatorVerdict,
	previousDraft?: BriefDraft,
): string {
	const sections: string[] = [
		`TASK:\n${task}`,
		soul   ? `SOUL (agent identity and voice):\n${soul}`   : "SOUL: (not defined)",
		rules  ? `RULES (hard constraints):\n${rules}`         : "RULES: (not defined)",
		duties ? `DUTIES (agent responsibilities):\n${duties}` : "DUTIES: (not defined)",
	];

	if (previousDraft) {
		sections.push(`PREVIOUS DRAFT (revise this — keep every assertion not flagged below unchanged, with the same id):\n${JSON.stringify(previousDraft)}`);
	}

	if (revisionFeedback) {
		const criticals = revisionFeedback.issues.filter(i => i.level === "critical");
		const warnings  = revisionFeedback.issues.filter(i => i.level === "warning");
		const feedbackParts: string[] = [
			`REVISION FEEDBACK (you must address all CRITICAL issues):`,
			`Summary: ${revisionFeedback.summary}`,
		];
		if (criticals.length > 0) {
			feedbackParts.push(
				`\nCRITICAL (must fix):\n${criticals.map(i => `- [Assertion ${i.assertion_id ?? "overall"}] ${i.issue}\n  Fix: ${i.fix}`).join("\n")}`,
			);
		}
		if (warnings.length > 0) {
			feedbackParts.push(
				`\nWARNINGS (should fix):\n${warnings.map(i => `- ${i.issue}`).join("\n")}`,
			);
		}
		sections.push(feedbackParts.join("\n"));
	}

	return sections.join("\n\n---\n\n");
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
	// Strip markdown code blocks if the model wraps output despite instructions
	const fenced = raw.match(/```(?:json)?\s*([\s\S]*?)```/);
	if (fenced) return fenced[1].trim();
	// Fallback: find first { ... } block
	const start = raw.indexOf("{");
	const end = raw.lastIndexOf("}");
	if (start !== -1 && end !== -1 && end > start) return raw.slice(start, end + 1);
	return raw.trim();
}

export async function runPlanner(opts: {
	input: string;
	model?: string;
	agentDir: string;
}): Promise<{ draft: BriefDraft; costs: SessionCosts }> {
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
			systemPrompt: PLANNER_SYSTEM_PROMPT,
			replaceBuiltinTools: true,
			maxTurns: 1,
		});

		try {
			const raw = await collectAssistantText(gen);
			const json = extractJson(raw);
			const parsed = JSON.parse(json) as BriefDraft;
			if (!parsed.assertions || !Array.isArray(parsed.assertions)) {
				throw new Error("Missing assertions array");
			}
			if (!parsed.rubric || typeof parsed.rubric !== "object") {
				throw new Error("Missing rubric object");
			}
			const MAX_ASSERTIONS = 15;
			if (parsed.assertions.length > MAX_ASSERTIONS) {
				console.warn(`[brief] Planner returned ${parsed.assertions.length} assertions — truncating to ${MAX_ASSERTIONS}`);
				parsed.assertions = parsed.assertions.slice(0, MAX_ASSERTIONS);
			}
			for (const [m, u] of Object.entries(gen.costs().modelUsage)) {
				costTracker.add(m, u);
			}
			return { draft: parsed, costs: costTracker.get() };
		} catch (err: any) {
			for (const [m, u] of Object.entries(gen.costs().modelUsage)) {
				costTracker.add(m, u);
			}
			if (attempt === 2) {
				throw new BriefGenerationError(`Planner failed to produce valid JSON after 2 attempts: ${err.message}`);
			}
		}
	}

	throw new BriefGenerationError("Planner failed");
}
