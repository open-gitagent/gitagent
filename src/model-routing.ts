// Classifies each SkillFlow step by complexity and resolves the model it should
// run on: lightweight tasks (summarize/extract/classify/transform) to a cheap
// model, reasoning-heavy tasks to the configured reasoning model. Explicit
// per-step / per-skill settings win; anything unresolved falls back to primary.

export type ModelTier = "lightweight" | "reasoning";

export interface RoutingConfig {
	/** Defaults to true when a routing block is present. */
	enabled?: boolean;
	/** Model id for lightweight tasks, e.g. "openai:gpt-4o-mini". */
	lightweight?: string;
	/** Model id for reasoning tasks, e.g. "openai:gpt-4o". */
	reasoning?: string;
	/** Classification overrides — first matching rule wins. */
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

export interface RouteResult {
	/** Resolved "provider:model" (undefined → let the runtime decide). */
	model?: string;
	/** Tier, when the model came from automatic classification. */
	tier: ModelTier | null;
	source: "step" | "skill" | "auto" | "fallback";
}

// Matched against word starts, so "summarize"/"summary"/"summarization" all hit
// "summ" without "already" matching "read".
const DEFAULT_LIGHTWEIGHT = [
	"summ", "extract", "classif", "transform", "format", "convert",
	"parse", "fetch", "read", "load", "lookup", "normaliz", "translat",
	"rephrase", "rewrite", "tag", "label", "render",
];
const DEFAULT_REASONING = [
	"search", "analy", "plan", "decid", "decision", "orchestrat", "solve",
	"reason", "validat", "evaluat", "review", "audit", "diagnos", "debug",
	"architect", "design", "strateg", "investigat", "assess", "judge",
	"verify", "critique", "infer", "deduc",
];

function matchesAny(text: string, keywords: string[]): boolean {
	for (const kw of keywords) {
		const re = new RegExp(`\\b${kw.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`, "i");
		if (re.test(text)) return true;
	}
	return false;
}

/**
 * Classify a task into a complexity tier. User rules take precedence over the
 * built-in defaults. A task that matches neither — or both — resolves to
 * "reasoning", so cost optimization never silently degrades quality.
 */
export function classifyTaskTier(
	classifyText: string,
	rules?: Array<{ tier: ModelTier; match: string[] }>,
): ModelTier {
	const text = classifyText || "";

	if (rules) {
		for (const rule of rules) {
			if (Array.isArray(rule.match) && matchesAny(text, rule.match)) {
				return rule.tier;
			}
		}
	}

	if (matchesAny(text, DEFAULT_REASONING)) return "reasoning";
	if (matchesAny(text, DEFAULT_LIGHTWEIGHT)) return "lightweight";
	return "reasoning";
}

/** Resolve a tier alias ("lightweight"/"reasoning") or pass a model id through. */
export function resolveModelAlias(ref: string | undefined, routing?: RoutingConfig): string | undefined {
	if (!ref) return undefined;
	if (ref === "lightweight") return routing?.lightweight || undefined;
	if (ref === "reasoning") return routing?.reasoning || undefined;
	return ref;
}

/**
 * Decide which model a task runs on, in precedence order: explicit per-step
 * model, per-skill model, automatic classification (only when a routing block
 * is present and enabled), then the primary model.
 */
export function resolveRoutedModel(input: RouteInput): RouteResult {
	const { stepModel, skillModel, classifyText, routing, primaryModel } = input;

	const fromStep = resolveModelAlias(stepModel, routing);
	if (fromStep) return { model: fromStep, tier: null, source: "step" };

	const fromSkill = resolveModelAlias(skillModel, routing);
	if (fromSkill) return { model: fromSkill, tier: null, source: "skill" };

	const autoEnabled = !!routing && routing.enabled !== false && !!(routing.lightweight || routing.reasoning);
	if (autoEnabled) {
		const tier = classifyTaskTier(classifyText, routing!.rules);
		const model = tier === "lightweight" ? routing!.lightweight : routing!.reasoning;
		if (model) return { model, tier, source: "auto" };
	}

	return { model: primaryModel, tier: null, source: "fallback" };
}
