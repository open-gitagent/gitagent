import type { Model } from "@mariozechner/pi-ai";
import type { GCAssistantMessage } from "../sdk-types.js";
import { runOneOffCompletion } from "./llm-call.js";

// ── Types ───────────────────────────────────────────────────────────────

export interface SkillRepairInput {
	skillDescription: string;
	/** Current numbered (or otherwise formatted) steps section of the skill. */
	currentSteps: string;
	/** Accumulated failure lessons that got this skill flagged. */
	negativeExamples: string[];
}

const REPAIR_TIMEOUT_MS = 20_000;
// Hard cap regardless of model verbosity — a repaired skill's steps section
// shouldn't be allowed to balloon indefinitely.
const MAX_REPAIR_CHARS = 3000;

const SYSTEM_PROMPT = `You are a skill-repair assistant for an autonomous coding agent's skill library.
You will be given a skill's description, its current steps, and the concrete
lessons learned from real failures while following those steps.

Rewrite the steps so they avoid every named failure mode while still
achieving the skill's description. Keep the steps generalizable — do not
hard-code project-specific paths, names, or values that only applied to one
past failure.

Output ONLY a bare numbered list of steps. No headers, no preamble, no
commentary, no markdown code fences.`;

function buildUserPrompt(input: SkillRepairInput): string {
	const lessonsText = input.negativeExamples.length
		? input.negativeExamples.map((n, i) => `${i + 1}. ${n}`).join("\n")
		: "(no specific lessons recorded)";
	return (
		`Skill description: ${input.skillDescription}\n\n` +
		`Current steps:\n${input.currentSteps}\n\n` +
		`Lessons learned from real failures:\n${lessonsText}\n\n` +
		`Write the repaired steps now.`
	);
}

function stripCodeFence(text: string): string {
	const fenced = text.match(/^```[a-z]*\r?\n([\s\S]*?)\r?\n```$/);
	return fenced ? fenced[1].trim() : text;
}

/**
 * Voyager-style skill repair: rewrites a flagged skill's steps using its own
 * accumulated Reflexion-style lessons, so a broken skill can be repaired by
 * the agent itself instead of requiring a human to rewrite it by hand.
 *
 * Unlike reflection (an invisible side-effect of task_tracker "end"), repair
 * is an explicit action the model chooses to call — errors here propagate
 * as normal tool errors rather than failing soft.
 */
export async function repairSkillSteps(
	model: Model<any>,
	input: SkillRepairInput,
	onUsage?: (msg: GCAssistantMessage) => void,
): Promise<string> {
	const text = await runOneOffCompletion(
		model,
		SYSTEM_PROMPT,
		buildUserPrompt(input),
		{ temperature: 0, maxTokens: 600, timeoutMs: REPAIR_TIMEOUT_MS },
		onUsage,
	);

	const cleaned = stripCodeFence(text.trim());
	if (cleaned.length > MAX_REPAIR_CHARS) {
		return cleaned.slice(0, MAX_REPAIR_CHARS - 1).trimEnd() + "…";
	}
	return cleaned;
}
