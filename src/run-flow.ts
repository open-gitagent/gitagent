// Batteries-included wrapper over executeFlow. The core loop stays agnostic
// about how a step runs so the CLI and the web UI can plug in their own
// behaviour — but the common case ("just run this flow in this agent") has an
// obvious answer, and callers shouldn't have to hand-write it. This supplies
// that default: each step is an isolated query() against the agent directory.
//
// Anything unusual (streaming to a browser, a custom model per step, a human
// approver) drops down to executeFlow directly.

import { join } from "path";
import { executeFlow, type FlowEvent, type FlowResult } from "./flow-runner.js";
import { discoverWorkflows, loadFlowDefinition, type SkillFlowDefinition } from "./workflows.js";
import { query } from "./sdk.js";

/**
 * What to do at an approval gate.
 * - "deny" (default): gates stop the flow. Safe for unattended runs.
 * - "auto": gates pass. Opt in deliberately — it disables the protection.
 * - function: ask a human however you like; return true to continue.
 */
export type ApprovalPolicy =
	| "deny"
	| "auto"
	| ((message: string, channel?: string) => Promise<boolean>);

export interface StepUsage {
	index: number;
	skill: string;
	/** Fresh (uncached) input tokens. With prompt caching on, this is small. */
	inputTokens: number;
	/** Input served from cache — often the bulk of a step's input. */
	cacheReadTokens: number;
	/** Input written into the cache this step. */
	cacheWriteTokens: number;
	outputTokens: number;
	/** input + cacheRead + cacheWrite + output — the whole billable volume. */
	totalTokens: number;
	requests: number;
	costUsd: number;
	/** Model ids that served this step, comma-joined. */
	models: string;
}

export interface RunFlowOptions {
	/** Agent directory — the one holding agent.yaml, skills/ and workflows/. */
	agentDir: string;
	/** Flow name (e.g. "daily-report") or an already-loaded definition. */
	flow: string | SkillFlowDefinition;
	/** Text substituted for {input} in step prompts. */
	input?: string;
	/** Override the agent's model for every step. */
	model?: string;
	/** Env profile passed through to the agent loader. */
	env?: string;
	/** Approval gate policy. Defaults to "deny". */
	approve?: ApprovalPolicy;
	/** Flow lifecycle events — same stream the CLI prints and the UI renders. */
	onProgress?: (event: FlowEvent) => void;
	/** Token usage for each step, reported as soon as that step finishes. */
	onStepUsage?: (usage: StepUsage) => void;
}

export interface RunFlowResult extends FlowResult {
	/** Per-step token usage, in step order. */
	usage: StepUsage[];
	/** Sum of every step's cost. */
	totalCostUsd: number;
	totalTokens: number;
}

function resolveApproval(policy: ApprovalPolicy | undefined) {
	if (typeof policy === "function") return policy;
	if (policy === "auto") return async () => true;
	// "deny" and undefined both mean: no handler, so executeFlow denies and
	// says why. Returning undefined keeps that message accurate.
	return undefined;
}

async function resolveFlow(agentDir: string, flow: string | SkillFlowDefinition): Promise<SkillFlowDefinition> {
	if (typeof flow !== "string") return flow;

	const workflows = await discoverWorkflows(agentDir);
	const meta = workflows.find((w) => w.name === flow);

	if (!meta) {
		const runnable = workflows.filter((w) => w.type === "flow").map((w) => w.name);
		throw new Error(
			`Flow "${flow}" not found in ${agentDir}/workflows` +
			(runnable.length ? ` — available: ${runnable.join(", ")}` : " — no runnable flows defined"),
		);
	}
	if (meta.type !== "flow") {
		throw new Error(`"${flow}" is a reference workflow (${meta.format}), not a runnable SkillFlow — it has no steps to execute`);
	}

	return loadFlowDefinition(join(agentDir, meta.filePath));
}

/**
 * Run a SkillFlow in an agent directory. Each step is an isolated agent call,
 * with the previous steps' output threaded into its prompt.
 *
 *   const r = await runFlow({ agentDir: "./my-agent", flow: "daily-report", input: "..." });
 *
 * Approval gates deny by default — pass `approve: "auto"` or a function to
 * change that.
 */
export async function runFlow(opts: RunFlowOptions): Promise<RunFlowResult> {
	const flow = await resolveFlow(opts.agentDir, opts.flow);
	const usage: StepUsage[] = [];

	const result = await executeFlow(flow, opts.input ?? "", {
		runStep: async (prompt, step, index) => {
			const q = query({ prompt, dir: opts.agentDir, model: opts.model, env: opts.env });

			let output = "";
			for await (const msg of q) {
				if (msg.type === "assistant" && msg.content) output += msg.content;
			}

			// One query per step, so this session's costs are this step's alone.
			// Cache counters only exist per-model — the session totals cover
			// fresh input and output only, so read them off modelUsage or a
			// cache-heavy step looks nearly free.
			const costs = q.costs();
			const models = Object.values(costs.modelUsage ?? {});
			const cacheReadTokens = models.reduce((n, m) => n + (m.cacheReadTokens ?? 0), 0);
			const cacheWriteTokens = models.reduce((n, m) => n + (m.cacheWriteTokens ?? 0), 0);

			const entry: StepUsage = {
				index,
				skill: step.skill,
				inputTokens: costs.totalInputTokens,
				cacheReadTokens,
				cacheWriteTokens,
				outputTokens: costs.totalOutputTokens,
				totalTokens: costs.totalInputTokens + cacheReadTokens + cacheWriteTokens + costs.totalOutputTokens,
				requests: costs.totalRequests,
				costUsd: costs.totalCostUsd,
				models: Object.keys(costs.modelUsage ?? {}).join(",") || "-",
			};
			usage.push(entry);
			opts.onStepUsage?.(entry);

			return output;
		},
		requestApproval: resolveApproval(opts.approve),
		onProgress: opts.onProgress,
	});

	return {
		...result,
		usage,
		totalCostUsd: usage.reduce((n, u) => n + u.costUsd, 0),
		totalTokens: usage.reduce((n, u) => n + u.totalTokens, 0),
	};
}
