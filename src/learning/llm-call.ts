import { Agent } from "@mariozechner/pi-agent-core";
import type { Model } from "@mariozechner/pi-ai";
import { recordGenAiCall } from "../telemetry.js";
import type { GCAssistantMessage } from "../sdk-types.js";

export interface OneOffCompletionOpts {
	temperature?: number;
	maxTokens?: number;
	timeoutMs?: number;
}

/**
 * Runs a single bare, tool-less completion outside the main agent loop
 * (used for Reflexion-style reflection and Voyager-style skill repair).
 * Forwards usage via `onUsage`, since these calls happen outside the main
 * loop's own message_end handler and would otherwise be invisible to the
 * caller's cost tracking.
 *
 * Throws on error, empty output, or timeout — callers decide whether to
 * fail-soft or propagate.
 */
export async function runOneOffCompletion(
	model: Model<any>,
	systemPrompt: string,
	userPrompt: string,
	opts: OneOffCompletionOpts = {},
	onUsage?: (msg: GCAssistantMessage) => void,
): Promise<string> {
	const { temperature = 0, maxTokens = 300, timeoutMs = 15_000 } = opts;

	const agent = new Agent({
		initialState: {
			systemPrompt,
			model,
			tools: [],
			temperature,
			maxTokens,
		} as any,
	});

	let collected = "";
	let failed: string | undefined;
	const startedAt = Date.now();
	agent.subscribe((event: any) => {
		if (event.type === "message_end" && event.message?.role === "assistant") {
			const msg = event.message;
			for (const block of msg.content) {
				if (block.type === "text") collected += block.text;
			}
			if (msg.stopReason === "error") failed = msg.errorMessage || "LLM call failed";
			recordGenAiCall(msg, { durationMs: Date.now() - startedAt });

			if (onUsage && msg.usage) {
				onUsage({
					type: "assistant",
					content: collected,
					model: msg.model ?? "unknown",
					provider: msg.provider ?? "unknown",
					stopReason: msg.stopReason ?? "stop",
					errorMessage: msg.errorMessage,
					usage: {
						inputTokens: msg.usage.input ?? 0,
						outputTokens: msg.usage.output ?? 0,
						cacheReadTokens: msg.usage.cacheRead ?? 0,
						cacheWriteTokens: msg.usage.cacheWrite ?? 0,
						totalTokens: msg.usage.totalTokens ?? 0,
						costUsd: msg.usage.cost?.total ?? 0,
					},
				});
			}
		}
	});

	const timer = setTimeout(() => {
		try {
			agent.abort();
		} catch {
			/* ignore */
		}
	}, timeoutMs);
	try {
		await agent.prompt(userPrompt);
	} finally {
		clearTimeout(timer);
	}

	if (failed) throw new Error(failed);
	const text = collected.trim();
	if (!text) throw new Error("LLM call produced empty output");
	return text;
}
