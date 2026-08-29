// Resolves which model each SkillFlow step runs on. Explicit per-step / per-skill
// settings win; otherwise the step is classified by one call to the lightweight
// model via an injected `query` (RouteDeps), falling back to a keyword heuristic
// when no `query` is supplied.

export type ModelTier = "lightweight" | "reasoning";

export interface RoutingConfig {
	/** Defaults to true when a routing block is present. */
	enabled?: boolean;
	/** Model id for lightweight tasks, e.g. "openai:gpt-4o-mini". */
	lightweight?: string;
	/** Model id for reasoning tasks, e.g. "openai:gpt-4o". */
	reasoning?: string;
	/** Classification overrides — first matching rule wins, before the LLM/keyword step. */
	rules?: Array<{ tier: ModelTier; match: string[] }>;
}

export interface RouteInput {
	/** Explicit per-step model (highest priority); alias or model id. */
	stepModel?: string;
	/** Per-skill default from SKILL.md frontmatter; alias or model id. */
	skillModel?: string;
	/** Text used to classify the task (skill name + step prompt). */
	classifyText: string;
	routing?: RoutingConfig;
	/** The agent's preferred model — the ultimate fallback. */
	primaryModel?: string;
}

/** Minimal shape of the SDK `query()` used to classify — injected so core stays decoupled and testable. */
export type RouteQuery = (opts: {
	prompt: string;
	model?: string;
	dir?: string;
	env?: string;
	systemPrompt?: string;
	maxTurns?: number;
	tools?: [];
	replaceBuiltinTools?: boolean;
}) => AsyncIterable<{ type: string; content?: string }>;

export interface RouteDeps {
	/** When present (and routing is enabled) classification runs one LLM call on the lightweight model. */
	query?: RouteQuery;
	dir?: string;
	env?: string;
}

export interface RouteResult {
	/** Resolved "provider:model" (undefined → let the runtime decide). */
	model?: string;
	/** Tier, when the model came from automatic classification. */
	tier: ModelTier | null;
	source: "step" | "skill" | "auto" | "fallback";
}

// Keyword fallback, used only when no `query` is injected. Ambiguous verbs like
// "search" that appear in both trivial and complex prompts are deliberately left
// out so they don't systematically overpay.
const DEFAULT_LIGHTWEIGHT = [
	"summ", "extract", "classif", "transform", "format", "convert",
	"parse", "fetch", "read", "load", "lookup", "normaliz", "translat",
	"rephrase", "rewrite", "tag", "label", "render",
];
const DEFAULT_REASONING = [
	"analy", "plan", "decid", "decision", "orchestrat", "solve",
	"reason", "validat", "evaluat", "review", "audit", "diagnos", "debug",
	"architect", "design", "strateg", "investigat", "research", "assess",
	"judge", "verify", "critique", "infer", "deduc",
];

function matchesAny(text: string, keywords: string[]): boolean {
	for (const kw of keywords) {
		const re = new RegExp(`\\b${kw.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`, "i");
		if (re.test(text)) return true;
	}
	return false;
}

function matchRules(text: string, rules?: Array<{ tier: ModelTier; match: string[] }>): ModelTier | null {
	if (rules) {
		for (const rule of rules) {
			if (Array.isArray(rule.match) && matchesAny(text, rule.match)) return rule.tier;
		}
	}
	return null;
}

/**
 * Keyword-based tier classification — the offline fallback. User rules win;
 * otherwise a task that matches neither list (or both) resolves to "reasoning",
 * so cost optimization never silently degrades quality.
 */
export function classifyByKeywords(
	classifyText: string,
	rules?: Array<{ tier: ModelTier; match: string[] }>,
): ModelTier {
	const text = classifyText || "";
	const ruled = matchRules(text, rules);
	if (ruled) return ruled;
	if (matchesAny(text, DEFAULT_REASONING)) return "reasoning";
	if (matchesAny(text, DEFAULT_LIGHTWEIGHT)) return "lightweight";
	return "reasoning";
}

const CLASSIFY_INSTRUCTIONS =
	`Classify the following agent task as exactly one word: "lightweight" or "reasoning".\n` +
	`- lightweight: mechanical work with a known shape (summarize, extract, format, fetch, read, look up).\n` +
	`- reasoning: needs analysis, planning, multi-step logic, or judgment.\n` +
	`If unsure, answer "reasoning". Reply with only the single word.\n\nTask: `;

/** One classification call on the lightweight model. Returns null if the call fails or is unparseable. */
async function classifyViaLLM(text: string, model: string, deps: RouteDeps): Promise<ModelTier | null> {
	try {
		// Constrained one-shot: no tools, single turn — keeps it a single cheap call.
		const result = deps.query!({
			prompt: CLASSIFY_INSTRUCTIONS + text.trim(),
			model,
			dir: deps.dir,
			env: deps.env,
			systemPrompt: "You are a task classifier. Reply with exactly one word.",
			maxTurns: 1,
			tools: [],
			replaceBuiltinTools: true,
		});
		let out = "";
		for await (const msg of result) {
			if (msg.type === "assistant" && msg.content) out += msg.content;
		}
		const answer = out.toLowerCase();
		if (answer.includes("lightweight")) return "lightweight";
		if (answer.includes("reason")) return "reasoning";
		return null;
	} catch {
		return null;
	}
}

/**
 * Resolve a tier alias ("lightweight"/"reasoning") or pass a model id through.
 * Warns when an alias is requested but the routing block has no model for that
 * tier, so silently falling through to the fallback stays observable.
 */
export function resolveModelAlias(ref: string | undefined, routing?: RoutingConfig): string | undefined {
	if (!ref) return undefined;
	if (ref === "lightweight" || ref === "reasoning") {
		const configured = routing?.[ref];
		if (!configured) {
			console.warn(`[routing] tier alias '${ref}' requested but routing.${ref} is not configured; falling through`);
			return undefined;
		}
		return configured;
	}
	return ref;
}

/**
 * Decide which model a task runs on, in precedence order: explicit per-step
 * model, per-skill model, automatic classification (only when a routing block
 * is present and enabled), then the primary model. Automatic classification
 * uses the injected `query` fn when available, else the keyword fallback.
 */
export async function resolveRoutedModel(input: RouteInput, deps: RouteDeps = {}): Promise<RouteResult> {
	const { stepModel, skillModel, classifyText, routing, primaryModel } = input;

	const fromStep = resolveModelAlias(stepModel, routing);
	if (fromStep) return { model: fromStep, tier: null, source: "step" };

	const fromSkill = resolveModelAlias(skillModel, routing);
	if (fromSkill) return { model: fromSkill, tier: null, source: "skill" };

	const autoEnabled = !!routing && routing.enabled !== false && !!(routing.lightweight || routing.reasoning);
	if (autoEnabled) {
		let tier = matchRules(classifyText || "", routing!.rules);
		if (!tier && deps.query && routing!.lightweight) {
			tier = await classifyViaLLM(classifyText, routing!.lightweight, deps);
		}
		if (!tier) tier = classifyByKeywords(classifyText, routing!.rules);
		const model = tier === "lightweight" ? routing!.lightweight : routing!.reasoning;
		if (model) return { model, tier, source: "auto" };
	}

	return { model: primaryModel, tier: null, source: "fallback" };
}
