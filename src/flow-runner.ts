// Executes a SkillFlow: runs each step in order, threading every step's output
// into the next one's prompt. All side effects are injected (RunFlowDeps) so the
// same loop drives the CLI, the scheduler, the SDK, and the voice web UI —
// the caller supplies how to run a step, how to report progress, and how to ask
// a human for approval.

import type { SkillFlowDefinition, SkillFlowStep } from "./workflows.js";

/** Reserved pseudo-skill: pauses the flow until a human approves. */
export const APPROVAL_GATE = "__approval_gate__";

export type FlowEvent =
	| { type: "flow_start"; flow: string; totalSteps: number }
	| { type: "step_start"; index: number; total: number; skill: string }
	| { type: "step_done"; index: number; total: number; skill: string; output: string }
	| { type: "approval_requested"; index: number; total: number; channel?: string }
	| { type: "approval_resolved"; index: number; approved: boolean }
	| { type: "flow_done"; flow: string; steps: number }
	| { type: "flow_aborted"; flow: string; index: number; reason: string };

export interface FlowRunDeps {
	/**
	 * Run one step and return the assistant's text. Receives the fully-built
	 * prompt plus the raw step and its index — so a caller can inspect the step
	 * for per-step settings (e.g. a future `model` field) without the loop
	 * needing to know about them.
	 */
	runStep: (prompt: string, step: SkillFlowStep, index: number) => Promise<string>;
	/** Progress reporting — terminal output, WebSocket frames, log lines. */
	onProgress?: (event: FlowEvent) => void;
	/**
	 * Ask a human to approve continuing. Omit it and approval gates DENY:
	 * a gate exists to guard a risky step, so with nobody to ask, the safe
	 * answer is to stop. Callers wanting unattended runs must opt in explicitly
	 * by passing `async () => true`.
	 */
	requestApproval?: (message: string, channel?: string) => Promise<boolean>;
}

export interface FlowStepResult {
	index: number;
	skill: string;
	output: string;
}

export interface FlowResult {
	flow: string;
	/** False when an approval gate denied, timed out, or had no handler. */
	completed: boolean;
	steps: FlowStepResult[];
	/** The accumulated context after the last step that ran. */
	context: string;
	/** 0-based index of the step that stopped the flow, when not completed. */
	abortedAt?: number;
	abortReason?: string;
}

function buildStepPrompt(step: SkillFlowStep, userContext: string, runningContext: string): string {
	return `Use the skill "${step.skill}" (load it with /skill:${step.skill}).
${step.prompt.replace(/\{input\}/g, userContext)}

Context from previous steps:
${runningContext}`;
}

function buildApprovalMessage(
	flow: SkillFlowDefinition,
	step: SkillFlowStep,
	index: number,
	runningContext: string,
): string {
	if (step.prompt) {
		return `⏸ Approval Required: ${step.prompt}\n\nReply YES to continue or NO to cancel.`;
	}
	return `⏸ Flow "${flow.name}" paused at step ${index + 1}/${flow.steps.length}.\n\nCompleted so far:\n${runningContext.slice(0, 500)}\n\nReply YES to continue or NO to cancel.`;
}

/**
 * Run a SkillFlow to completion, or stop at the first denied approval gate.
 * Never throws on a denied gate — that's a normal outcome, reported via
 * `completed: false`. Errors from `runStep` propagate to the caller.
 */
export async function executeFlow(
	flow: SkillFlowDefinition,
	userContext: string,
	deps: FlowRunDeps,
): Promise<FlowResult> {
	const total = flow.steps.length;
	const results: FlowStepResult[] = [];
	let runningContext = userContext;

	deps.onProgress?.({ type: "flow_start", flow: flow.name, totalSteps: total });

	for (let i = 0; i < total; i++) {
		const step = flow.steps[i];

		if (step.skill === APPROVAL_GATE) {
			const channel = step.channel;
			deps.onProgress?.({ type: "approval_requested", index: i, total, channel });

			// No handler → deny. See RunFlowDeps.requestApproval.
			const approved = deps.requestApproval
				? await deps.requestApproval(buildApprovalMessage(flow, step, i, runningContext), channel)
				: false;

			deps.onProgress?.({ type: "approval_resolved", index: i, approved });

			if (!approved) {
				const reason = deps.requestApproval
					? `Approval denied at step ${i + 1}/${total}`
					: `Approval gate at step ${i + 1}/${total} could not be delivered — no approval handler available`;
				deps.onProgress?.({ type: "flow_aborted", flow: flow.name, index: i, reason });
				return { flow: flow.name, completed: false, steps: results, context: runningContext, abortedAt: i, abortReason: reason };
			}

			runningContext += `\n\n[Step ${i + 1}: approval gate]: Approved${channel ? ` via ${channel}` : ""}`;
			continue;
		}

		deps.onProgress?.({ type: "step_start", index: i, total, skill: step.skill });

		const output = await deps.runStep(buildStepPrompt(step, userContext, runningContext), step, i);

		results.push({ index: i, skill: step.skill, output });
		runningContext += `\n\n[Step ${i + 1} result (${step.skill})]: ${output}`;

		deps.onProgress?.({ type: "step_done", index: i, total, skill: step.skill, output });
	}

	deps.onProgress?.({ type: "flow_done", flow: flow.name, steps: results.length });
	return { flow: flow.name, completed: true, steps: results, context: runningContext };
}
