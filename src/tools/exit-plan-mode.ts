import type { AgentTool } from "@mariozechner/pi-agent-core";
import { buildTool } from "../tool-factory.js";
import type { PermissionState, PlanOutcome } from "../permissions.js";

/**
 * The exit_plan_mode tool. The model calls it once it has finished read-only
 * research in plan mode. It emits the proposed plan to the consumer, then
 * BLOCKS awaiting host approval (query.approvePlan / query.rejectPlan resolve
 * the deferred on state). On approval the shared PermissionState.mode is
 * flipped so subsequent tool calls are allowed. Mirrors Claude Code's
 * ExitPlanMode, but driven entirely through the SDK rather than a TUI.
 */
export function createExitPlanModeTool(
	state: PermissionState,
	emitPlan: (plan: string) => void,
): AgentTool<any> {
	return buildTool({
		name: "exit_plan_mode",
		description:
			"Call this ONLY when you are in plan mode and have finished researching. " +
			"Provide your implementation plan (markdown) for the user to approve. No " +
			"changes are made until the user approves. Do not call this outside plan mode.",
		parameters: {
			properties: {
				plan: {
					type: "string",
					description: "The implementation plan in markdown for the user to review.",
				},
			},
			required: ["plan"],
		},
		metadata: { isReadOnly: true },
		execute: async (args: { plan: string }) => {
			const plan = args?.plan ?? "";

			// Register the deferred before emitting so a synchronous consumer
			// (e.g. an interactive CLI approver) can resolve it immediately.
			let resolve!: (o: PlanOutcome) => void;
			const promise = new Promise<PlanOutcome>((r) => {
				resolve = r;
			});
			state.planDeferred = { plan, promise, resolve };

			emitPlan(plan);

			const outcome = await promise;
			state.planDeferred = null;

			if (outcome.approved) {
				state.mode = outcome.nextMode ?? "acceptEdits";
				return "Plan approved by the user. Proceed with implementing the approved plan now.";
			}
			state.mode = "plan";
			return (
				`Plan rejected by the user. Feedback: ${outcome.feedback ?? "(none)"}. ` +
				`Revise your plan based on the feedback and call exit_plan_mode again.`
			);
		},
	});
}
