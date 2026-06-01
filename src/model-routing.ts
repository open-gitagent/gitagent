// Auto Model Routing (issue #48)
//
// Classifies each task in an agent workflow by complexity and routes it to the
// most appropriate model: lightweight tasks (summarize/extract/classify/
// transform) go to a cheap model, while reasoning-intensive tasks (search,
// planning, decision-making, tool orchestration, complex problem solving)
// stay on the configured reasoning model. Explicit per-step / per-skill model
// settings always win, and anything unresolved falls back to the primary model.

export type ModelTier = "lightweight" | "reasoning";

export interface RoutingConfig {
	/** Master switch. Defaults to true when a routing block is present. */
	enabled?: boolean;
	/** Concrete model id for lightweight tasks, e.g. "openai:gpt-4o-mini". */
	lightweight?: string;
	/** Concrete model id for reasoning tasks, e.g. "openai:gpt-4o". */
	reasoning?: string;
	/** User overrides for classification — first matching rule wins. */
	rules?: Array<{ tier: ModelTier; match: string[] }>;
}

export interface RouteInput {
	/** Explicit per-step model (highest priority). May be an alias or model id. */
	stepModel?: string;
	/** Per-skill default model from SKILL.md frontmatter. May be an alias or id. */
	skillModel?: string;
	/** Text used to classify the task (typically skill name + step prompt). */
	classifyText: string;
	/** Routing configuration from agent.yaml (model.routing). */
	routing?: RoutingConfig;
	/** The agent's primary/preferred model — the ultimate fallback. */
	primaryModel?: string;
}

export interface RouteResult {
	/** Resolved concrete "provider:model" string (undefined → let runtime decide). */
	model?: string;
	/** The complexity tier, when the model came from automatic classification. */
	tier: ModelTier | null;
	/** Where the decision came from. */
	source: "step" | "skill" | "auto" | "fallback";
}

// Default task-to-tier keyword framework, derived directly from the issue's
// recommended task-type table. Matched against word starts so "summarize",
// "summary" and "summarization" all hit "summ", without false positives like
// "already" matching "read".
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
		// Word-start boundary: keyword must begin a word.
		const re = new RegExp(`\\b${kw.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`, "i");
		if (re.test(text)) return true;
	}
	return false;
}

/**
 * Classify a task into a complexity tier. User-defined rules (from
 * model.routing.rules) take precedence over the built-in defaults. When a task
 * matches neither — or matches both — it defaults to "reasoning" so that
 * reasoning quality is never sacrificed to save cost.
 */
export function classifyTaskTier(
	classifyText: string,
	rules?: Array<{ tier: ModelTier; match: string[] }>,
): ModelTier {
	const text = classifyText || "";

	// User overrides first, in declaration order.
	if (rules) {
		for (const rule of rules) {
			if (Array.isArray(rule.match) && matchesAny(text, rule.match)) {
				return rule.tier;
			}
		}
	}

	const hasReasoning = matchesAny(text, DEFAULT_REASONING);
	if (hasReasoning) return "reasoning";
	const hasLightweight = matchesAny(text, DEFAULT_LIGHTWEIGHT);
	if (hasLightweight) return "lightweight";

	// Unknown → keep quality high.
	return "reasoning";
}

/**
 * Resolve a model reference that may be a routing-tier alias
 * ("lightweight"/"reasoning") or a literal "provider:model" id.
 */
export function resolveModelAlias(ref: string | undefined, routing?: RoutingConfig): string | undefined {
	if (!ref) return undefined;
	if (ref === "lightweight") return routing?.lightweight || undefined;
	if (ref === "reasoning") return routing?.reasoning || undefined;
	return ref;
}

/**
 * Decide which model a task should run on. Precedence:
 *   1. explicit per-step model      (source: "step")
 *   2. per-skill declared model      (source: "skill")
 *   3. automatic classification      (source: "auto")  — when routing is enabled
 *   4. primary/preferred model       (source: "fallback")
 *
 * Automatic routing is active only when a routing block is present and not
 * disabled. If classification picks a tier with no configured model, it falls
 * through to the primary model (fallback on routing failure).
 */
export function resolveRoutedModel(input: RouteInput): RouteResult {
	const { stepModel, skillModel, classifyText, routing, primaryModel } = input;

	// 1. Explicit per-step override.
	const fromStep = resolveModelAlias(stepModel, routing);
	if (fromStep) return { model: fromStep, tier: null, source: "step" };

	// 2. Per-skill declared default.
	const fromSkill = resolveModelAlias(skillModel, routing);
	if (fromSkill) return { model: fromSkill, tier: null, source: "skill" };

	// 3. Automatic classification (opt-in via a routing block).
	const autoEnabled = !!routing && routing.enabled !== false && !!(routing.lightweight || routing.reasoning);
	if (autoEnabled) {
		const tier = classifyTaskTier(classifyText, routing!.rules);
		const model = tier === "lightweight" ? routing!.lightweight : routing!.reasoning;
		if (model) return { model, tier, source: "auto" };
	}

	// 4. Fallback to the primary model.
	return { model: primaryModel, tier: null, source: "fallback" };
}
