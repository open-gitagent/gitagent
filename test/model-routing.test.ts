// Tests for the resolution priority chain (step > skill > auto > fallback) and
// the classifier's safety default (ambiguous tasks resolve to reasoning).

import test from "node:test";
import assert from "node:assert/strict";

import {
	classifyTaskTier,
	resolveModelAlias,
	resolveRoutedModel,
	type RoutingConfig,
} from "../src/model-routing.ts";

const routing: RoutingConfig = {
	lightweight: "openai:gpt-4o-mini",
	reasoning: "openai:gpt-4o",
};

// ── classifyTaskTier ───────────────────────────────────────────────────

test("classifies lightweight task types as lightweight", () => {
	assert.equal(classifyTaskTier("summarize the pull request diff"), "lightweight");
	assert.equal(classifyTaskTier("extract the linked issue number"), "lightweight");
	assert.equal(classifyTaskTier("format the report as markdown"), "lightweight");
});

test("classifies reasoning task types as reasoning", () => {
	assert.equal(classifyTaskTier("analyze the security implications"), "reasoning");
	assert.equal(classifyTaskTier("plan a multi-step remediation"), "reasoning");
	assert.equal(classifyTaskTier("validate the truth score"), "reasoning");
});

test("unknown tasks default to reasoning (never silently downgrade quality)", () => {
	assert.equal(classifyTaskTier("frobnicate the widget"), "reasoning");
	assert.equal(classifyTaskTier(""), "reasoning");
});

test("a task matching both tiers resolves to reasoning", () => {
	// "summarize" (lightweight) + "analyze" (reasoning) → reasoning wins.
	assert.equal(classifyTaskTier("summarize and analyze the results"), "reasoning");
});

test("user rules take precedence over the built-in defaults", () => {
	// "analyze" would default to reasoning, but a user rule forces lightweight.
	const rules = [{ tier: "lightweight" as const, match: ["analyze"] }];
	assert.equal(classifyTaskTier("analyze the log lines", rules), "lightweight");
});

// ── resolveModelAlias ──────────────────────────────────────────────────

test("resolves tier aliases and passes literal model ids through", () => {
	assert.equal(resolveModelAlias("lightweight", routing), "openai:gpt-4o-mini");
	assert.equal(resolveModelAlias("reasoning", routing), "openai:gpt-4o");
	assert.equal(resolveModelAlias("anthropic:claude-sonnet-4-5", routing), "anthropic:claude-sonnet-4-5");
	assert.equal(resolveModelAlias(undefined, routing), undefined);
});

// ── resolveRoutedModel priority chain ──────────────────────────────────

test("explicit per-step model wins over everything", () => {
	const r = resolveRoutedModel({
		stepModel: "openai:gpt-4o",
		skillModel: "openai:gpt-4o-mini",
		classifyText: "summarize the diff",
		routing,
		primaryModel: "openai:gpt-5-reasoning",
	});
	assert.equal(r.model, "openai:gpt-4o");
	assert.equal(r.source, "step");
});

test("per-skill model wins when no step model is set", () => {
	const r = resolveRoutedModel({
		skillModel: "lightweight",
		classifyText: "analyze the diff",
		routing,
		primaryModel: "openai:gpt-5-reasoning",
	});
	assert.equal(r.model, "openai:gpt-4o-mini");
	assert.equal(r.source, "skill");
});

test("auto classification routes lightweight tasks to the cheap model", () => {
	const r = resolveRoutedModel({
		classifyText: "summarize the pull request",
		routing,
		primaryModel: "openai:gpt-5-reasoning",
	});
	assert.equal(r.model, "openai:gpt-4o-mini");
	assert.equal(r.tier, "lightweight");
	assert.equal(r.source, "auto");
});

test("routing stays opt-in — no routing block falls back to primary", () => {
	const r = resolveRoutedModel({
		classifyText: "summarize the pull request",
		primaryModel: "openai:gpt-5-reasoning",
	});
	assert.equal(r.model, "openai:gpt-5-reasoning");
	assert.equal(r.source, "fallback");
});

test("disabled routing falls back to primary", () => {
	const r = resolveRoutedModel({
		classifyText: "summarize the pull request",
		routing: { ...routing, enabled: false },
		primaryModel: "openai:gpt-5-reasoning",
	});
	assert.equal(r.model, "openai:gpt-5-reasoning");
	assert.equal(r.source, "fallback");
});
