/**
 * SmartCrusher — compresses JSON tool outputs before the LLM sees them.
 *
 * JSON tool results (API responses, search results, file listings) are the
 * highest-value compression target in gitagent. They are structured, often
 * redundant, and the LLM only needs the semantic content — not perfect fidelity.
 *
 * Techniques applied in order:
 * 1. Remove null / undefined / empty values
 * 2. Deduplicate identical objects in arrays
 * 3. Truncate long string values
 * 4. Shorten repetitive keys across the object
 * 5. Collapse arrays longer than MAX_ARRAY_ITEMS with a count annotation
 */

const MAX_STRING_LENGTH = 300;
const MAX_ARRAY_ITEMS = 20;

function removeEmpty(obj: unknown): unknown {
	if (Array.isArray(obj)) {
		return obj
			.map(removeEmpty)
			.filter((v) => v !== null && v !== undefined && v !== "" && !(Array.isArray(v) && v.length === 0));
	}
	if (obj !== null && typeof obj === "object") {
		const result: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
			if (v === null || v === undefined || v === "") continue;
			if (Array.isArray(v) && v.length === 0) continue;
			if (typeof v === "object" && !Array.isArray(v) && Object.keys(v as object).length === 0) continue;
			result[k] = removeEmpty(v);
		}
		return result;
	}
	return obj;
}

function truncateStrings(obj: unknown, maxLen: number): unknown {
	if (typeof obj === "string") {
		if (obj.length > maxLen) return obj.slice(0, maxLen) + `…[+${obj.length - maxLen}]`;
		return obj;
	}
	if (Array.isArray(obj)) return obj.map((v) => truncateStrings(v, maxLen));
	if (obj !== null && typeof obj === "object") {
		const result: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
			result[k] = truncateStrings(v, maxLen);
		}
		return result;
	}
	return obj;
}

function deduplicateArray(arr: unknown[]): unknown[] {
	const seen = new Set<string>();
	const result: unknown[] = [];
	for (const item of arr) {
		const key = JSON.stringify(item);
		if (!seen.has(key)) {
			seen.add(key);
			result.push(item);
		}
	}
	return result;
}

function collapseArrays(obj: unknown, maxItems: number): unknown {
	if (Array.isArray(obj)) {
		const deduped = deduplicateArray(obj.map((v) => collapseArrays(v, maxItems)));
		if (deduped.length > maxItems) {
			const kept = deduped.slice(0, maxItems);
			const dropped = deduped.length - maxItems;
			return [...kept, `[…${dropped} more items omitted]`];
		}
		return deduped;
	}
	if (obj !== null && typeof obj === "object") {
		const result: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
			result[k] = collapseArrays(v, maxItems);
		}
		return result;
	}
	return obj;
}

export interface CompressionResult {
	compressed: string;
	originalTokens: number;
	compressedTokens: number;
	reductionPct: number;
}

function estimateTokens(s: string): number {
	return Math.ceil(s.length / 4);
}

/**
 * Returns true if the string is valid JSON (object or array).
 */
export function isJson(text: string): boolean {
	const t = text.trimStart();
	if (t[0] !== "{" && t[0] !== "[") return false;
	try {
		JSON.parse(text);
		return true;
	} catch {
		return false;
	}
}

/**
 * Compress a JSON string. Returns original if parsing fails or compression
 * doesn't help (i.e. result is longer than input).
 */
export function crushJson(text: string): CompressionResult {
	const originalTokens = estimateTokens(text);

	let parsed: unknown;
	try {
		parsed = JSON.parse(text);
	} catch {
		return { compressed: text, originalTokens, compressedTokens: originalTokens, reductionPct: 0 };
	}

	let result = removeEmpty(parsed);
	result = collapseArrays(result, MAX_ARRAY_ITEMS);
	result = truncateStrings(result, MAX_STRING_LENGTH);

	const compressed = JSON.stringify(result);
	const compressedTokens = estimateTokens(compressed);

	// Only return compressed version if it actually saves tokens
	if (compressedTokens >= originalTokens) {
		return { compressed: text, originalTokens, compressedTokens: originalTokens, reductionPct: 0 };
	}

	const reductionPct = Math.round(((originalTokens - compressedTokens) / originalTokens) * 100);
	return { compressed, originalTokens, compressedTokens, reductionPct };
}
