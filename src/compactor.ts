/**
 * Context-window compaction for the agent loop — wired via pi-agent-core's
 * `transformContext` hook. Ports the strategy from rust/gitagent-rs/src/pi/compact.rs.
 *
 * Two-stage strategy:
 *   1. Fold the aged head into a summary user message; keep the recent tail verbatim.
 *   2. Fallback: truncate old tool-result / assistant-text content in place.
 *
 * Invariant: never DROP a message in the truncate path — that can orphan an
 * assistant toolCall from its toolResult (→ provider 400). Folding replaces a
 * contiguous head so no pair is split.
 */

import type { AgentMessage } from "@mariozechner/pi-agent-core";
import type {
	AssistantMessage,
	TextContent,
	ToolResultMessage,
	UserMessage,
} from "@mariozechner/pi-ai";

export type CompactorOptions = {
	/** Model context window in tokens (from model.contextWindow). */
	contextWindow: number;
	/** Fraction of the window used as the send budget. Default 0.75. */
	budgetRatio?: number;
	/** Fraction of the window kept as a recent verbatim tail. Default 0.40. */
	recentRatio?: number;
	/**
	 * When true, fold aged turns into a summary message (extractive by default,
	 * or via `summarizer` when provided). When false, truncation-only.
	 * Default true.
	 */
	summarize?: boolean;
	/** Chars-per-token heuristic. Default 3.5 (closer than 4 for JSON/code). */
	charsPerToken?: number;
	/** Optional LLM summarizer. Receives a prompt; returns summary text. */
	summarizer?: (prompt: string, signal?: AbortSignal) => Promise<string>;
};

export type CompactorStatus = {
	maxTokens: number;
	recentTokens: number;
	lastUsageInput: number;
	folded: number;
};

const SUMMARY_PREFIX =
	"[Summary of the earlier conversation, compacted to fit the context window]\n";

function isUser(m: AgentMessage): m is UserMessage {
	return (m as UserMessage).role === "user";
}

function isAssistant(m: AgentMessage): m is AssistantMessage {
	return (m as AssistantMessage).role === "assistant";
}

function isToolResult(m: AgentMessage): m is ToolResultMessage {
	return (m as ToolResultMessage).role === "toolResult";
}

function textFromContent(
	content: string | { type: string; text?: string }[],
): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content
		.filter((b): b is TextContent => b.type === "text" && typeof b.text === "string")
		.map((b) => b.text)
		.join("");
}

function setToolResultText(msg: ToolResultMessage, text: string): ToolResultMessage {
	return {
		...msg,
		content: [{ type: "text", text }],
	};
}

function mapAssistantText(
	msg: AssistantMessage,
	map: (text: string) => string,
): AssistantMessage {
	return {
		...msg,
		content: msg.content.map((b) => {
			if (b.type === "text") return { ...b, text: map(b.text) };
			return b;
		}),
	};
}

function truncateMiddle(s: string, keep: number): string {
	const n = [...s].length;
	if (n <= keep * 2 + 40) return s;
	const chars = [...s];
	const head = chars.slice(0, keep).join("");
	const tail = chars.slice(-keep).join("");
	return `${head}\n… [${n - keep * 2} chars omitted to fit the context window] …\n${tail}`;
}

function msgChars(m: AgentMessage): number {
	if (isUser(m)) {
		return textFromContent(m.content).length;
	}
	if (isToolResult(m)) {
		return textFromContent(m.content).length + 32;
	}
	if (isAssistant(m)) {
		let n = 0;
		for (const b of m.content) {
			if (b.type === "text" || b.type === "thinking") {
				n += (b.type === "text" ? b.text : b.thinking).length;
			} else if (b.type === "toolCall") {
				n += JSON.stringify(b.arguments ?? {}).length + 48;
			}
		}
		return n;
	}
	return 0;
}

function cloneMessages(messages: AgentMessage[]): AgentMessage[] {
	return messages.map((m) => {
		if (isAssistant(m)) {
			return { ...m, content: m.content.map((b) => ({ ...b })) };
		}
		if (isToolResult(m)) {
			return { ...m, content: m.content.map((b) => ({ ...b })) };
		}
		if (isUser(m)) {
			if (typeof m.content === "string") return { ...m };
			return { ...m, content: m.content.map((b) => ({ ...b })) };
		}
		return m;
	});
}

function renderForSummary(messages: AgentMessage[]): string {
	const parts: string[] = [];
	for (const m of messages) {
		if (isUser(m)) {
			parts.push(`User: ${textFromContent(m.content)}`);
		} else if (isAssistant(m)) {
			const text = m.content
				.filter((b): b is TextContent => b.type === "text")
				.map((b) => b.text)
				.join("");
			if (text) parts.push(`Assistant: ${text}`);
			for (const b of m.content) {
				if (b.type === "toolCall") {
					const args = truncateMiddle(JSON.stringify(b.arguments ?? {}), 100);
					parts.push(`Tool call: ${b.name}(${args})`);
				}
			}
		} else if (isToolResult(m)) {
			parts.push(
				`Tool result [${m.toolName}]: ${truncateMiddle(textFromContent(m.content), 250)}`,
			);
		}
	}
	return parts.join("\n");
}

export class Compactor {
	readonly maxTokens: number;
	readonly recentTokens: number;
	private readonly summarize: boolean;
	private readonly charsPerToken: number;
	private readonly summarizer?: CompactorOptions["summarizer"];

	/** Leading messages already folded into `summaryText`. */
	private folded = 0;
	private summaryText = "";
	/** Last observed provider usage.input — floors the estimate when larger. */
	private lastUsageInput = 0;

	constructor(opts: CompactorOptions) {
		const budgetRatio = opts.budgetRatio ?? 0.75;
		const recentRatio = opts.recentRatio ?? 0.4;
		this.maxTokens = Math.floor(opts.contextWindow * budgetRatio);
		this.recentTokens = Math.floor(opts.contextWindow * recentRatio);
		this.summarize = opts.summarize ?? true;
		this.charsPerToken = opts.charsPerToken ?? 3.5;
		this.summarizer = opts.summarizer;
	}

	status(): CompactorStatus {
		return {
			maxTokens: this.maxTokens,
			recentTokens: this.recentTokens,
			lastUsageInput: this.lastUsageInput,
			folded: this.folded,
		};
	}

	/** Record provider usage.input from the last assistant turn (overflow guard). */
	observeUsage(inputTokens: number): void {
		if (Number.isFinite(inputTokens) && inputTokens > 0) {
			this.lastUsageInput = Math.max(this.lastUsageInput, Math.floor(inputTokens));
		}
	}

	reset(): void {
		this.folded = 0;
		this.summaryText = "";
		this.lastUsageInput = 0;
	}

	estimateTokens(messages: AgentMessage[]): number {
		const chars = messages.reduce((n, m) => n + msgChars(m), 0);
		return Math.ceil(chars / this.charsPerToken);
	}

	/** Effective size: char estimate floored by last real usage.input when present. */
	effectiveTokens(messages: AgentMessage[]): number {
		return Math.max(this.estimateTokens(messages), this.lastUsageInput);
	}

	/** True when char estimate or last usage.input exceeds the send budget. */
	needsCompact(messages: AgentMessage[]): boolean {
		return this.effectiveTokens(messages) > this.maxTokens;
	}

	/**
	 * Synchronous truncation-only compaction — safe fallback.
	 * Never drops messages (length preserved).
	 * Loop exit uses char estimate only (usage.input is a trigger, not a floor
	 * that would prevent ever getting under budget after truncation).
	 */
	apply(messages: AgentMessage[]): AgentMessage[] {
		let out = cloneMessages(messages);
		if (this.estimateTokens(out) <= this.maxTokens) {
			// Content fits; drop stale usage floor from a prior oversized turn.
			this.lastUsageInput = 0;
			return out;
		}

		// Pass 1: truncate tool-result content, oldest-first.
		for (let i = 0; i < out.length; i++) {
			if (this.estimateTokens(out) <= this.maxTokens) break;
			const m = out[i];
			if (isToolResult(m)) {
				const text = textFromContent(m.content);
				if ([...text].length > 840) {
					out[i] = setToolResultText(m, truncateMiddle(text, 400));
				}
			}
		}

		// Pass 2: compress assistant text if still over.
		for (let i = 0; i < out.length; i++) {
			if (this.estimateTokens(out) <= this.maxTokens) break;
			const m = out[i];
			if (isAssistant(m)) {
				out[i] = mapAssistantText(m, (txt) =>
					[...txt].length > 640 ? truncateMiddle(txt, 300) : txt,
				);
			}
		}

		// Pass 3: last resort — more aggressive tool truncation.
		for (let i = 0; i < out.length; i++) {
			if (this.estimateTokens(out) <= this.maxTokens) break;
			const m = out[i];
			if (isToolResult(m)) {
				const text = textFromContent(m.content);
				if ([...text].length > 200) {
					out[i] = setToolResultText(m, truncateMiddle(text, 80));
				}
			}
		}

		// Compaction succeeded for this view — clear usage floor so the next
		// under-budget identity path works until the provider reports again.
		this.lastUsageInput = 0;
		return out;
	}

	/**
	 * Full compaction: summarize/fold aged head, keep recent tail, then truncate
	 * if still over. Must not throw — callers wrap as well.
	 */
	async compact(
		messages: AgentMessage[],
		signal?: AbortSignal,
	): Promise<AgentMessage[]> {
		if (!this.needsCompact(messages)) {
			return messages;
		}
		if (!this.summarize) {
			return this.apply(messages);
		}

		// Choose recent tail by walking from the end.
		let tailStart = messages.length;
		let acc = 0;
		while (tailStart > 0) {
			const cost = Math.ceil(msgChars(messages[tailStart - 1]) / this.charsPerToken);
			if (acc + cost > this.recentTokens) break;
			acc += cost;
			tailStart -= 1;
		}
		// Never let the tail begin on a toolResult (its toolCall would be stranded).
		while (tailStart < messages.length && isToolResult(messages[tailStart])) {
			tailStart += 1;
		}
		tailStart = Math.min(tailStart, Math.max(0, messages.length - 1));
		if (tailStart === 0) {
			return this.apply(messages);
		}

		const head = messages.slice(0, tailStart);
		const prior = this.summaryText;
		const folded = Math.min(this.folded, head.length);
		const newSlice = head.slice(folded);

		let summary: string;
		if (newSlice.length === 0 && prior) {
			summary = prior;
		} else {
			summary = await this.foldHead(prior, newSlice, signal);
			this.summaryText = summary;
			this.folded = head.length;
		}

		let out: AgentMessage[] = [
			{
				role: "user",
				content: SUMMARY_PREFIX + summary,
				timestamp: Date.now(),
			},
			...messages.slice(tailStart),
		];

		if (this.estimateTokens(out) > this.maxTokens) {
			out = this.apply(out);
		} else {
			this.lastUsageInput = 0;
		}
		return out;
	}

	private async foldHead(
		prior: string,
		newSlice: AgentMessage[],
		signal?: AbortSignal,
	): Promise<string> {
		const convo = truncateMiddle(renderForSummary(newSlice), 4000);
		const existing = prior ? `EXISTING SUMMARY:\n${prior}\n\n` : "";
		const prompt =
			"Update the running summary with the new exchange below. Preserve key " +
			"decisions, file paths, code changes, commands run, errors, and outcomes; " +
			"omit routine tool-call detail unless it failed. Output ONLY the updated summary.\n\n" +
			`${existing}NEW EXCHANGE:\n${convo}`;

		if (this.summarizer) {
			try {
				const text = (await this.summarizer(prompt, signal)).trim();
				if (text) return text;
			} catch {
				/* fall through to extractive */
			}
			if (prior) return prior;
		}

		// Extractive fallback (no LLM): keep prior + truncated new exchange.
		const extractive = prior
			? `${prior}\n\n---\n${convo}`
			: convo;
		return truncateMiddle(extractive, 3000) || "(summary unavailable)";
	}
}

/**
 * Build a transformContext callback that never throws (pi-agent-core contract).
 */
export function createTransformContext(compactor: Compactor) {
	return async (
		messages: AgentMessage[],
		signal?: AbortSignal,
	): Promise<AgentMessage[]> => {
		try {
			return await compactor.compact(messages, signal);
		} catch {
			return messages;
		}
	};
}
