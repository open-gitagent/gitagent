// ── Per-model cost and token tracking ──────────────────────────────────

export interface ModelUsage {
	inputTokens: number;
	outputTokens: number;
	cacheReadTokens: number;
	cacheWriteTokens: number;
	totalTokens: number;
	costUsd: number;
	requests: number;
}

export interface SessionCosts {
	totalCostUsd: number;
	totalInputTokens: number;
	totalOutputTokens: number;
	totalRequests: number;
	startTime: number;
	modelUsage: Record<string, ModelUsage>;
	/**
	 * False when at least one request consumed tokens but no pricing was
	 * available to price it (custom/unregistered model with no cost table).
	 * Lets callers tell "cost was $0" apart from "cost is unknown" — the
	 * root confusion behind `costs()` reporting 0 (issue #67).
	 */
	costDataAvailable: boolean;
}

/** Per-million-token pricing, as carried on a pi-ai Model's `cost` field. */
export interface ModelCostRates {
	input: number;
	output: number;
	cacheRead: number;
	cacheWrite: number;
}

export interface TokenCounts {
	inputTokens: number;
	outputTokens: number;
	cacheReadTokens?: number;
	cacheWriteTokens?: number;
}

/**
 * Compute the USD cost of a request from token counts and the model's
 * per-million-token rates (same formula pi-ai uses internally).
 *
 * Returns `null` when the model carries no pricing (all rates 0) yet tokens
 * were actually consumed — i.e. the cost is *unknown*, not zero. Returns a
 * number (possibly 0) otherwise.
 */
export function computeCostUsd(usage: TokenCounts, rates: ModelCostRates): number | null {
	const input = usage.inputTokens ?? 0;
	const output = usage.outputTokens ?? 0;
	const cacheRead = usage.cacheReadTokens ?? 0;
	const cacheWrite = usage.cacheWriteTokens ?? 0;

	const hasRates =
		rates.input > 0 || rates.output > 0 || rates.cacheRead > 0 || rates.cacheWrite > 0;
	if (!hasRates) {
		const tokens = input + output + cacheRead + cacheWrite;
		return tokens > 0 ? null : 0;
	}

	return (
		(rates.input / 1_000_000) * input +
		(rates.output / 1_000_000) * output +
		(rates.cacheRead / 1_000_000) * cacheRead +
		(rates.cacheWrite / 1_000_000) * cacheWrite
	);
}

/**
 * Tracks token usage and cost per model across a session.
 * Mirrors Claude Code's cost-tracker pattern.
 */
export class CostTracker {
	private costs: SessionCosts;

	constructor() {
		this.costs = {
			totalCostUsd: 0,
			totalInputTokens: 0,
			totalOutputTokens: 0,
			totalRequests: 0,
			startTime: Date.now(),
			modelUsage: {},
			costDataAvailable: true,
		};
	}

	add(
		model: string,
		usage: {
			inputTokens: number;
			outputTokens: number;
			cacheReadTokens?: number;
			cacheWriteTokens?: number;
			totalTokens?: number;
			costUsd?: number;
		},
		/** Pass false when tokens were used but cost could not be priced. */
		costResolved: boolean = true,
	): void {
		if (!costResolved) this.costs.costDataAvailable = false;
		this.costs.totalInputTokens += usage.inputTokens;
		this.costs.totalOutputTokens += usage.outputTokens;
		this.costs.totalCostUsd += usage.costUsd ?? 0;
		this.costs.totalRequests++;

		if (!this.costs.modelUsage[model]) {
			this.costs.modelUsage[model] = {
				inputTokens: 0,
				outputTokens: 0,
				cacheReadTokens: 0,
				cacheWriteTokens: 0,
				totalTokens: 0,
				costUsd: 0,
				requests: 0,
			};
		}
		const mu = this.costs.modelUsage[model];
		mu.inputTokens += usage.inputTokens;
		mu.outputTokens += usage.outputTokens;
		mu.cacheReadTokens += usage.cacheReadTokens ?? 0;
		mu.cacheWriteTokens += usage.cacheWriteTokens ?? 0;
		mu.totalTokens += usage.totalTokens ?? (usage.inputTokens + usage.outputTokens);
		mu.costUsd += usage.costUsd ?? 0;
		mu.requests++;
	}

	get(): SessionCosts {
		return {
			...this.costs,
			modelUsage: { ...this.costs.modelUsage },
		};
	}

	reset(): void {
		this.costs = {
			totalCostUsd: 0,
			totalInputTokens: 0,
			totalOutputTokens: 0,
			totalRequests: 0,
			startTime: Date.now(),
			modelUsage: {},
			costDataAvailable: true,
		};
	}
}
