import type { Model } from "@mariozechner/pi-ai";
import type { GCAssistantMessage } from "../sdk-types.js";
import { runOneOffCompletion } from "./llm-call.js";

// ── Types ───────────────────────────────────────────────────────────────

export interface ReflectionInput {
	objective: string;
	/** Ordered step descriptions actually recorded for the failed attempt. */
	steps: string[];
	/** The model's own raw one-line failure report, if it gave one. */
	failureReason?: string;
}

const REFLECTION_TIMEOUT_MS = 15_000;
// Hard cap regardless of model verbosity — protects the 10-slot
// negative_examples array from unbounded growth.
const MAX_REFLECTION_CHARS = 500;

const SYSTEM_PROMPT = `You are a terse failure-analysis assistant for an autonomous coding agent.
You will be given a task objective, the ordered steps the agent actually
took, and how the agent itself described the failure.

Write EXACTLY ONE plain-text paragraph, 2-4 sentences, under 400 characters,
with no markdown, no headers, no bullet points, no line breaks, and no
preamble ("Root cause:", "Here is my analysis", etc.). The paragraph must:
1. State the most likely root cause, grounded in the specific steps shown
   (not generic advice like "check for errors").
2. State one concrete, different strategy to try on the next attempt.

Do not restate the objective. Output only the paragraph.`;

function buildUserPrompt(input: ReflectionInput): string {
	const stepsText = input.steps.length
		? input.steps.map((s, i) => `${i + 1}. ${s}`).join("\n")
		: "(no steps were recorded)";
	return (
		`Objective: ${input.objective}\n\n` +
		`Steps taken (in order):\n${stepsText}\n\n` +
		`Reported outcome: failure — "${input.failureReason || "not specified"}"\n\n` +
		`Write the reflection now.`
	);
}

/**
 * Reflexion-style verbal reflection on a failed skill-using attempt. Grounds
 * the reflection in the attempt's actual recorded steps, not just the
 * model's own one-line failure report, so retries get an analyzed lesson
 * instead of a rephrased complaint.
 *
 * Fails soft: any error (timeout, empty output, model unavailable) throws,
 * and the caller is expected to fall back to the raw failure reason. This
 * must never be the reason a task_tracker "end" call fails.
 */
export async function reflectOnFailure(
	model: Model<any>,
	input: ReflectionInput,
	onUsage?: (msg: GCAssistantMessage) => void,
): Promise<string> {
	const text = await runOneOffCompletion(
		model,
		SYSTEM_PROMPT,
		buildUserPrompt(input),
		{ temperature: 0, maxTokens: 300, timeoutMs: REFLECTION_TIMEOUT_MS },
		onUsage,
	);

	if (text.length > MAX_REFLECTION_CHARS) {
		return text.slice(0, MAX_REFLECTION_CHARS - 1).trimEnd() + "…";
	}
	return text;
}
