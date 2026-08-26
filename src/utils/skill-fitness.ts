// Advisory second pass over a generated workflow, checking whether each step's
// chosen skill actually suits the step's task.
//
// The installed-skill check in schemas.ts answers "does this skill exist", which
// is a hard yes/no. This answers the fuzzier "is this skill right for the job" —
// a workflow can name only real skills and still wire a weather briefing into a
// company-announcements skill. That is deliberately reported as a warning rather
// than folded into the retry loop: the judgement is subjective, and a false
// positive must never block a workflow the user is happy with.

import type { SkillMetadata } from "../skills.js";
import { APPROVAL_SKILL, type WorkflowDef, type WorkflowStep } from "./schemas.js";
import {
	DEFAULT_MODEL,
	defaultLlmClient,
	type LlmClient,
	type LlmMessage,
} from "./workflow-generator.js";

export interface FitnessWarning {
	/** 0-based index into the workflow's steps array. */
	stepIndex: number;
	skill: string;
	reason: string;
}

export interface CheckSkillFitnessOptions {
	workflow: WorkflowDef;
	skills: SkillMetadata[];
	model?: string;
	apiKey?: string;
	llm?: LlmClient;
}

const FITNESS_SYSTEM = `You are reviewing an already-valid workflow for skill-selection mistakes.

Every step below names a real, installed skill. Your only job is to spot steps where the chosen skill's description is clearly unrelated to what the step actually asks for — cases where the workflow will run without error but do the wrong thing.

Be conservative. Flag a step ONLY when the mismatch is obvious from the description. If the skill could plausibly cover the task, or its description is broad or vague enough to include it, do not flag it. Returning nothing is the correct answer for most workflows.

Never flag a step whose description is marked "never flag this one".

Output format — return ONLY a JSON array, no prose and no markdown fences:
[{"step": 2, "reason": "one sentence naming what the skill is for and what the step actually needs"}]

Return [] when every step is a reasonable fit.`;

export function buildFitnessPrompt(workflow: WorkflowDef, skills: SkillMetadata[]): string {
	const byName = new Map(skills.map((s) => [s.name, s]));
	const steps = Array.isArray(workflow?.steps) ? workflow.steps : [];
	const blocks = steps.map((step: any, i: number) => {
		const name = typeof step?.skill === "string" ? step.skill : "";
		const prompt = typeof step?.prompt === "string" ? step.prompt : "";
		const meta = byName.get(name);
		// Steps we cannot judge still occupy their index, so they are listed to keep
		// the model's "step N" references aligned with the real steps array.
		const description =
			name === APPROVAL_SKILL
				? "(built-in human-approval step, not an installed skill — never flag this one)"
				: meta
					? meta.description
					: "(not installed — reported separately — never flag this one)";
		return `step ${i}:\n  skill: ${name}\n  skill is for: ${description}\n  step asks for: ${prompt}`;
	});
	return `Workflow: ${workflow?.name ?? "(unnamed)"}
Stated goal: ${workflow?.description ?? "(none)"}

${blocks.join("\n\n")}`;
}

// The response should be a bare JSON array, but models like to wrap it in prose
// or a fence. Grab the outermost bracketed span rather than anchoring.
const JSON_ARRAY_RE = /\[[\s\S]*\]/;

/**
 * Tolerant parse of the fitness response. Anything unexpected — prose, a fence,
 * malformed JSON, out-of-range indices — degrades to "no warnings" rather than
 * throwing, because this check is advisory and must not fail generation.
 */
export function parseFitnessResponse(raw: string, steps: WorkflowStep[]): FitnessWarning[] {
	if (!raw || !raw.trim()) return [];
	const match = raw.match(JSON_ARRAY_RE);
	if (!match) return [];

	let parsed: unknown;
	try {
		parsed = JSON.parse(match[0]);
	} catch {
		return [];
	}
	if (!Array.isArray(parsed)) return [];

	const warnings: FitnessWarning[] = [];
	const seen = new Set<number>();
	for (const entry of parsed) {
		if (!entry || typeof entry !== "object" || Array.isArray(entry)) continue;
		const index = (entry as any).step;
		const reason = (entry as any).reason;
		if (typeof index !== "number" || !Number.isInteger(index)) continue;
		// A hallucinated index would point at a step that does not exist, so the
		// warning could not be rendered against anything. Drop it.
		if (index < 0 || index >= steps.length) continue;
		if (typeof reason !== "string" || !reason.trim()) continue;
		if (seen.has(index)) continue;
		seen.add(index);
		const skill = steps[index] as any;
		warnings.push({
			stepIndex: index,
			skill: typeof skill?.skill === "string" ? skill.skill : "",
			reason: reason.trim(),
		});
	}
	return warnings.sort((a, b) => a.stepIndex - b.stepIndex);
}

export async function checkSkillFitness(opts: CheckSkillFitnessOptions): Promise<FitnessWarning[]> {
	const steps = Array.isArray(opts.workflow?.steps) ? opts.workflow.steps : [];
	if (steps.length === 0 || opts.skills.length === 0) return [];

	// Only a step naming an installed skill can be judged — there is no
	// description to compare against otherwise, and 'approval' is a pseudo-skill
	// with no SKILL.md at all. If nothing is judgeable, skip the LLM call.
	const installed = new Set(opts.skills.map((s) => s.name));
	const judgeable = steps.some(
		(s: any) => typeof s?.skill === "string" && s.skill !== APPROVAL_SKILL && installed.has(s.skill),
	);
	if (!judgeable) return [];

	const llm = opts.llm ?? defaultLlmClient;
	const messages: LlmMessage[] = [
		{ role: "system", content: FITNESS_SYSTEM },
		{ role: "user", content: buildFitnessPrompt(opts.workflow, opts.skills) },
	];

	let raw: string;
	try {
		raw = await llm(messages, {
			model: opts.model ?? DEFAULT_MODEL,
			apiKey: opts.apiKey,
			temperature: 0,
		});
	} catch {
		// A provider error here means we lose the warning, not the workflow.
		return [];
	}
	return parseFitnessResponse(raw, steps);
}
