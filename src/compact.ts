import type { GCMessage } from "./sdk-types.js";

// ── Token estimation ──────────────────────────────────────────────────

/** Rough token estimate: 1 token ≈ 4 chars */
export function estimateTokens(text: string): number {
	return Math.ceil(text.length / 4);
}

/** Estimate total tokens across a message array */
export function estimateMessageTokens(messages: GCMessage[]): number {
	let total = 0;
	for (const msg of messages) {
		switch (msg.type) {
			case "assistant":
				total += estimateTokens(msg.content) + estimateTokens(msg.thinking ?? "");
				break;
			case "user":
				total += estimateTokens(msg.content);
				break;
			case "tool_use":
				total += estimateTokens(JSON.stringify(msg.args)) + 50;
				break;
			case "tool_result":
				total += estimateTokens(msg.content);
				break;
			case "delta":
				// Streaming fragments are already counted via the final assistant
				// message — do not double-count (G16).
				break;
			case "system":
				total += estimateTokens(msg.content);
				break;
		}
	}
	return total;
}

// ── Compaction checks ─────────────────────────────────────────────────

/**
 * Check if messages are approaching context limit and need compaction.
 * Pass `usageInput` (last provider usage.input) to floor the estimate when the
 * real count is higher than the chars/4 heuristic (G16).
 */
export function needsCompaction(
	messages: GCMessage[],
	contextWindow: number = 200000,
	usageInput?: number,
): { needed: boolean; tokenEstimate: number; ratio: number } {
	const estimated = estimateMessageTokens(messages);
	const tokenEstimate =
		usageInput != null && usageInput > estimated ? usageInput : estimated;
	const ratio = tokenEstimate / contextWindow;
	return { needed: ratio > 0.75, tokenEstimate, ratio };
}

// ── Tool result truncation ────────────────────────────────────────────

/** Truncate oversized tool results, keeping first and last portions */
export function truncateToolResults(
	messages: GCMessage[],
	maxCharsPerResult: number = 10000,
): GCMessage[] {
	return messages.map((msg) => {
		if (msg.type === "tool_result" && msg.content.length > maxCharsPerResult) {
			const half = Math.floor(maxCharsPerResult / 2);
			const truncated =
				msg.content.slice(0, half) +
				`\n\n... [${msg.content.length - maxCharsPerResult} chars truncated] ...\n\n` +
				msg.content.slice(-half);
			return { ...msg, content: truncated };
		}
		return msg;
	});
}

// ── Conversation summarization ────────────────────────────────────────

/**
 * Build a text representation of messages for summarization.
 * Strips deltas and system messages, keeps the substantive conversation.
 */
export function messagesToText(messages: GCMessage[]): string {
	const parts: string[] = [];
	for (const msg of messages) {
		switch (msg.type) {
			case "assistant":
				parts.push(`Assistant: ${msg.content}`);
				break;
			case "user":
				parts.push(`User: ${msg.content}`);
				break;
			case "tool_use":
				parts.push(`Tool call: ${msg.toolName}(${JSON.stringify(msg.args).slice(0, 200)})`);
				break;
			case "tool_result":
				parts.push(`Tool result [${msg.toolName}]: ${msg.content.slice(0, 500)}`);
				break;
		}
	}
	return parts.join("\n");
}

/**
 * Generate a compaction prompt that can be sent to the model to summarize
 * the conversation so far. The caller runs the actual query.
 */
export function buildCompactPrompt(messages: GCMessage[]): string {
	const text = messagesToText(messages);
	if (!text) return "";
	return (
		"Summarize this conversation concisely. Preserve key decisions, " +
		"file paths, code changes, and outcomes. Omit tool call details " +
		"unless they failed.\n\n" +
		text
	);
}
