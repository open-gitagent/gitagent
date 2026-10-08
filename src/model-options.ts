import { streamSimple } from "@mariozechner/pi-ai";
import type { Agent, AgentEvent, StreamFn } from "@mariozechner/pi-agent-core";

/** Sampling options gitagent supports in `model.constraints`. */
export interface SamplingOptions {
	temperature?: number;
	maxTokens?: number;
	topP?: number;
	topK?: number;
	stopSequences?: string[];
}

/**
 * Normalize constraints written in either snake_case (agent.yaml) or
 * camelCase (SDK options) into SamplingOptions.
 */
export function samplingFromConstraints(constraints: Record<string, any> | undefined | null): SamplingOptions {
	const out: SamplingOptions = {};
	if (!constraints) return out;
	const c = constraints;
	if (c.temperature !== undefined) out.temperature = c.temperature;
	if (c.maxTokens !== undefined) out.maxTokens = c.maxTokens;
	if (c.max_tokens !== undefined) out.maxTokens = c.max_tokens;
	if (c.topP !== undefined) out.topP = c.topP;
	if (c.top_p !== undefined) out.topP = c.top_p;
	if (c.topK !== undefined) out.topK = c.topK;
	if (c.top_k !== undefined) out.topK = c.top_k;
	if (c.stopSequences !== undefined) out.stopSequences = c.stopSequences;
	if (c.stop_sequences !== undefined) out.stopSequences = c.stop_sequences;
	return out;
}

const warned = new Set<string>();
function warnOnce(key: string, msg: string): void {
	if (warned.has(key)) return;
	warned.add(key);
	console.warn(msg);
}

/**
 * pi-ai only has first-class options for temperature and maxTokens. topP,
 * topK and stop sequences are injected into the provider payload for the
 * APIs whose request shape we know; for other APIs a warning is printed once.
 */
export function applyExtraSampling(api: string, payload: any, s: SamplingOptions): any {
	if (!payload || typeof payload !== "object") return payload;
	const { topP, topK, stopSequences } = s;
	const unsupported = (field: string) =>
		warnOnce(`${api}:${field}`, `Warning: model constraint "${field}" is not supported for API "${api}" and will be ignored.`);

	switch (api) {
		case "anthropic-messages":
			if (topP !== undefined) payload.top_p = topP;
			if (topK !== undefined) payload.top_k = topK;
			if (stopSequences !== undefined) payload.stop_sequences = stopSequences;
			break;
		case "openai-completions":
			if (topP !== undefined) payload.top_p = topP;
			if (stopSequences !== undefined) payload.stop = stopSequences;
			if (topK !== undefined) unsupported("top_k");
			break;
		case "openai-responses":
		case "azure-openai-responses":
			if (topP !== undefined) payload.top_p = topP;
			if (topK !== undefined) unsupported("top_k");
			if (stopSequences !== undefined) unsupported("stop_sequences");
			break;
		case "google-generative-ai":
		case "google-vertex":
			payload.config = payload.config ?? {};
			if (topP !== undefined) payload.config.topP = topP;
			if (topK !== undefined) payload.config.topK = topK;
			if (stopSequences !== undefined) payload.config.stopSequences = stopSequences;
			break;
		default:
			if (topP !== undefined) unsupported("top_p");
			if (topK !== undefined) unsupported("top_k");
			if (stopSequences !== undefined) unsupported("stop_sequences");
	}
	return payload;
}

/**
 * Build a StreamFn that forwards sampling options to every LLM call.
 * pi-agent-core's Agent ignores these if they are put in `initialState`.
 */
export function createSamplingStreamFn(sampling: SamplingOptions, base: StreamFn = streamSimple): StreamFn | undefined {
	const hasAny = Object.values(sampling).some((v) => v !== undefined);
	if (!hasAny) return undefined;
	const needsPayload = sampling.topP !== undefined || sampling.topK !== undefined || sampling.stopSequences !== undefined;

	return (model, context, options) => {
		const next: any = { ...options };
		if (sampling.temperature !== undefined) next.temperature = sampling.temperature;
		if (sampling.maxTokens !== undefined) next.maxTokens = sampling.maxTokens;
		if (needsPayload) {
			const inner = options?.onPayload;
			next.onPayload = async (payload: unknown, m: any) => {
				const modified = applyExtraSampling(m?.api ?? model.api, payload, sampling);
				const replaced = inner ? await inner(modified, m) : undefined;
				return replaced ?? modified;
			};
		}
		return base(model, context, next);
	};
}

/**
 * Enforce a per-prompt turn limit. Counts turns since the last agent_start
 * and aborts the agent when a new turn would exceed `maxTurns`.
 * Returns the unsubscribe function.
 */
export function enforceMaxTurns(
	agent: Pick<Agent, "subscribe" | "abort">,
	maxTurns: number | undefined,
	onLimit?: (maxTurns: number) => void,
): () => void {
	if (maxTurns === undefined || maxTurns === null || !Number.isFinite(maxTurns) || maxTurns <= 0) {
		return () => {};
	}
	let turns = 0;
	return agent.subscribe((event: AgentEvent) => {
		if (event.type === "agent_start") {
			turns = 0;
		} else if (event.type === "turn_start") {
			turns++;
			if (turns > maxTurns) {
				onLimit?.(maxTurns);
				agent.abort();
			}
		}
	});
}

export function maxTurnsMessage(maxTurns: number): string {
	return `Stopped: reached max_turns limit (${maxTurns}).`;
}
