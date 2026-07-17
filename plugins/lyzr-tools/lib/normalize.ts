// Tool name normalization.
//
// gitagent tool names should be stable, collision-resistant identifiers.
// Every Lyzr-backed tool is prefixed with "lyzr_" so it can never collide
// with a local skill/tool of the same base name (e.g. a local "gmail" tool
// vs. Lyzr's "gmail" provider) — this is the RCA's "auto-prefixing" dedupe
// mitigation (see docs/lyzr-tool-auth-rca.md, Phase 4).

export function normalizeToolName(raw: string): string {
	const cleaned = raw
		.normalize("NFKD")
		.replace(/[̀-ͯ]/g, "") // strip diacritics
		.replace(/[^\w\s-]/g, "")
		.trim()
		.replace(/[\s-]+/g, "_")
		.toLowerCase();

	if (!cleaned) return "lyzr_tool";

	const withPrefix = cleaned.startsWith("lyzr_") ? cleaned : `lyzr_${cleaned}`;
	const result = withPrefix.replace(/_+/g, "_").replace(/^_+|_+$/g, "");
	return result === "lyzr" || !result ? "lyzr_tool" : result;
}

/** Normalize a free-form provider/server label into a stable lowercase key. */
export function normalizeProviderKey(name: string): string {
	return name
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, "_")
		.replace(/^_+|_+$/g, "");
}
