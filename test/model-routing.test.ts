// Tests for the resolution priority chain (step > skill > auto > fallback), the
// LLM classifier (via an injected fake query), the keyword fallback, and a
// save -> load roundtrip proving step `model` survives the YAML plumbing.

import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
	classifyByKeywords,
	resolveModelAlias,
	resolveRoutedModel,
	type RoutingConfig,
	type RouteQuery,
} from "../src/model-routing.ts";
import { saveFlowDefinition, loadFlowDefinition } from "../src/workflows.ts";

const routing: RoutingConfig = {
	lightweight: "openai:gpt-4o-mini",
	reasoning: "openai:gpt-4o",
};

// A fake injected query that always "answers" with the given text.
function fakeQuery(answer: string): RouteQuery {
	return () => (async function* () {
		yield { type: "assistant", content: answer };
	})();
}

// ── classifyByKeywords (offline fallback) ──────────────────────────────

test("keyword fallback classifies lightweight and reasoning verbs", () => {
	assert.equal(classifyByKeywords("summarize the pull request diff"), "lightweight");
	assert.equal(classifyByKeywords("analyze the security implications"), "reasoning");
});

test("unknown tasks default to reasoning (never silently downgrade quality)", () => {
	assert.equal(classifyByKeywords("frobnicate the widget"), "reasoning");
	assert.equal(classifyByKeywords(""), "reasoning");
});

test("bare 'search' no longer forces reasoning, but 'research' still does", () => {
	// Previously "search" was in the reasoning list, so this whole step went
	// expensive; now the lightweight "summarize" verb wins.
	assert.equal(classifyByKeywords("search the logs and summarize them"), "lightweight");
	assert.equal(classifyByKeywords("research the root cause"), "reasoning");
	assert.equal(classifyByKeywords("investigate the regression"), "reasoning");
});

test("user rules take precedence over defaults", () => {
	const rules = [{ tier: "lightweight" as const, match: ["analyze"] }];
	assert.equal(classifyByKeywords("analyze the log lines", rules), "lightweight");
});

// ── resolveModelAlias ──────────────────────────────────────────────────

test("resolves tier aliases and passes literal model ids through", () => {
	assert.equal(resolveModelAlias("lightweight", routing), "openai:gpt-4o-mini");
	assert.equal(resolveModelAlias("reasoning", routing), "openai:gpt-4o");
	assert.equal(resolveModelAlias("anthropic:claude-sonnet-4-5", routing), "anthropic:claude-sonnet-4-5");
	assert.equal(resolveModelAlias(undefined, routing), undefined);
});

test("alias with no configured tier model warns and returns undefined", () => {
	const warnings: string[] = [];
	const orig = console.warn;
	console.warn = (m?: any) => { warnings.push(String(m)); };
	try {
		assert.equal(resolveModelAlias("lightweight", { reasoning: "openai:gpt-4o" }), undefined);
	} finally {
		console.warn = orig;
	}
	assert.equal(warnings.length, 1);
	assert.match(warnings[0], /routing\.lightweight is not configured/);
});

// ── resolveRoutedModel priority chain ──────────────────────────────────

test("explicit per-step model wins over everything", async () => {
	const r = await resolveRoutedModel({
		stepModel: "openai:gpt-4o",
		skillModel: "openai:gpt-4o-mini",
		classifyText: "summarize the diff",
		routing,
		primaryModel: "openai:gpt-5-reasoning",
	});
	assert.equal(r.model, "openai:gpt-4o");
	assert.equal(r.source, "step");
});

test("per-skill model wins when no step model is set", async () => {
	const r = await resolveRoutedModel({
		skillModel: "lightweight",
		classifyText: "analyze the diff",
		routing,
		primaryModel: "openai:gpt-5-reasoning",
	});
	assert.equal(r.model, "openai:gpt-4o-mini");
	assert.equal(r.source, "skill");
});

test("auto routing uses the injected LLM classifier", async () => {
	const r = await resolveRoutedModel(
		{ classifyText: "do the thing", routing, primaryModel: "openai:gpt-5-reasoning" },
		{ query: fakeQuery("lightweight") },
	);
	assert.equal(r.model, "openai:gpt-4o-mini");
	assert.equal(r.tier, "lightweight");
	assert.equal(r.source, "auto");
});

test("LLM classifier answering 'reasoning' routes to the reasoning model", async () => {
	const r = await resolveRoutedModel(
		{ classifyText: "do the thing", routing, primaryModel: "openai:gpt-5-reasoning" },
		{ query: fakeQuery("reasoning") },
	);
	assert.equal(r.model, "openai:gpt-4o");
	assert.equal(r.tier, "reasoning");
});

test("unparseable LLM output falls back to the keyword heuristic", async () => {
	const r = await resolveRoutedModel(
		{ classifyText: "xyzzy", routing, primaryModel: "openai:gpt-5-reasoning" },
		{ query: fakeQuery("¯\\_(ツ)_/¯") },
	);
	assert.equal(r.tier, "reasoning"); // keyword fallback → unknown defaults to reasoning
	assert.equal(r.source, "auto");
});

test("no injected query → keyword fallback still routes", async () => {
	const r = await resolveRoutedModel({
		classifyText: "summarize the pull request",
		routing,
		primaryModel: "openai:gpt-5-reasoning",
	});
	assert.equal(r.model, "openai:gpt-4o-mini");
	assert.equal(r.source, "auto");
});

test("routing stays opt-in — no routing block falls back to primary", async () => {
	const r = await resolveRoutedModel({
		classifyText: "summarize the pull request",
		primaryModel: "openai:gpt-5-reasoning",
	});
	assert.equal(r.model, "openai:gpt-5-reasoning");
	assert.equal(r.source, "fallback");
});

test("disabled routing falls back to primary", async () => {
	const r = await resolveRoutedModel(
		{ classifyText: "summarize the pull request", routing: { ...routing, enabled: false }, primaryModel: "openai:gpt-5-reasoning" },
		{ query: fakeQuery("lightweight") },
	);
	assert.equal(r.model, "openai:gpt-5-reasoning");
	assert.equal(r.source, "fallback");
});

// ── Integration: step `model` survives the YAML roundtrip ──────────────

test("step model survives saveFlowDefinition -> loadFlowDefinition", async () => {
	const dir = await mkdtemp(join(tmpdir(), "gitagent-flow-"));
	try {
		const filePath = await saveFlowDefinition(dir, {
			name: "route-test",
			description: "roundtrip",
			steps: [
				{ skill: "summarize", prompt: "summarize {input}", model: "openai:gpt-4o-mini" },
				{ skill: "plan", prompt: "plan the fix", model: "reasoning" },
				{ skill: "noop", prompt: "no model here" },
			],
		});
		const loaded = await loadFlowDefinition(filePath);
		assert.equal(loaded.steps[0].model, "openai:gpt-4o-mini");
		assert.equal(loaded.steps[1].model, "reasoning");
		assert.equal(loaded.steps[2].model, undefined);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});
