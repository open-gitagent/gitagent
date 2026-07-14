/**
 * CacheAligner — stabilizes the system prompt prefix to maximize KV cache hits.
 *
 * Anthropic and OpenAI both cache the prefix of the system prompt. If the prefix
 * is identical across requests, the provider reuses the cached KV state and you
 * pay zero input tokens for it. If anything changes at the top — even a timestamp
 * or a dynamic memory line — the cache misses and you pay full price.
 *
 * The fix: put all STATIC content first (SOUL.md, RULES.md, knowledge, skills),
 * and all DYNAMIC content last (memory, current task, recent conversation).
 *
 * This module assembles a system prompt in that order and reports the stable
 * prefix length so callers can log or track cache effectiveness.
 */

export interface SystemPromptParts {
	/** Static — never changes between sessions (SOUL.md, RULES.md) */
	identity: string;
	/** Static — knowledge files marked always_load */
	knowledge: string;
	/** Static — skill definitions loaded for this session */
	skills: string;
	/** Dynamic — changes every session (memory, conversation summary) */
	memory: string;
	/** Dynamic — changes every turn */
	task: string;
}

export interface AlignedPrompt {
	/** The full assembled system prompt */
	prompt: string;
	/** Character index where the static prefix ends and dynamic content begins */
	staticPrefixEnd: number;
	/** Estimated tokens in the static prefix (eligible for KV cache) */
	staticTokens: number;
	/** Estimated tokens in the dynamic suffix (never cached) */
	dynamicTokens: number;
}

function estimateTokens(s: string): number {
	return Math.ceil(s.length / 4);
}

function section(header: string, content: string): string {
	if (!content.trim()) return "";
	return `${header}\n\n${content.trim()}`;
}

/**
 * Assembles a system prompt with static parts first, dynamic parts last.
 * Static parts are eligible for provider-side KV cache reuse.
 */
export function alignSystemPrompt(parts: SystemPromptParts): AlignedPrompt {
	const staticSections = [
		section("# Identity", parts.identity),
		section("# Knowledge", parts.knowledge),
		section("# Skills", parts.skills),
	].filter(Boolean);

	const dynamicSections = [
		section("# Memory", parts.memory),
		section("# Current Task", parts.task),
	].filter(Boolean);

	const staticPrefix = staticSections.join("\n\n");
	const dynamicSuffix = dynamicSections.join("\n\n");

	const prompt = dynamicSuffix
		? `${staticPrefix}\n\n${dynamicSuffix}`
		: staticPrefix;

	const staticPrefixEnd = staticPrefix.length;
	const staticTokens = estimateTokens(staticPrefix);
	const dynamicTokens = estimateTokens(dynamicSuffix);

	return { prompt, staticPrefixEnd, staticTokens, dynamicTokens };
}

/**
 * Returns the cache efficiency ratio: what fraction of the prompt is static.
 * 1.0 = fully cacheable, 0.0 = nothing cacheable.
 */
export function cacheEfficiency(aligned: AlignedPrompt): number {
	const total = aligned.staticTokens + aligned.dynamicTokens;
	if (total === 0) return 0;
	return aligned.staticTokens / total;
}
