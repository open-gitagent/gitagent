// Telemetry coverage for the workflow subcommand path.
//
// The subcommand used to return from main() before initTelemetry() ran, so
// nothing on this path was traced. These tests assert the two things that were
// wrong once it is traced: the per-call gen_ai spans must parent to the
// generation span (otherwise the cost of one run cannot be summed), and token
// attributes must account for cache tokens (otherwise a 1500-token call reports
// as 3 while the cost attribute disagrees).

import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { InMemorySpanExporter, NodeTracerProvider, SimpleSpanProcessor } from "@opentelemetry/sdk-trace-node";

import { initTelemetry, recordGenAiCall } from "../src/telemetry.ts";
import { runGenerate } from "../src/commands/workflow.ts";
import type { LlmClient } from "../src/utils/workflow-generator.ts";

const exporter = new InMemorySpanExporter();
const provider = new NodeTracerProvider({ spanProcessors: [new SimpleSpanProcessor(exporter)] });
await initTelemetry({ _testProvider: provider });

const VALID_YAML = `name: morning-digest
description: Summarize unread emails and post to Slack each morning.
steps:
  - skill: gmail
    prompt: Fetch unread emails.
  - skill: slack
    prompt: Post the digest.
`;

// Shaped like a real pi-ai assistant message: prompt caching puts the bulk of
// the input in cacheWrite, leaving `input` tiny.
const FAKE_MSG = {
	role: "assistant",
	provider: "anthropic",
	model: "claude-sonnet-4-6",
	stopReason: "stop",
	usage: {
		input: 3,
		output: 179,
		cacheRead: 11,
		cacheWrite: 1509,
		cost: { total: 0.008574 },
	},
};

async function withAgentDir(fn: (dir: string) => Promise<void>): Promise<void> {
	const dir = await mkdtemp(join(tmpdir(), "gitagent-otel-"));
	try {
		for (const name of ["gmail", "slack"]) {
			await mkdir(join(dir, "skills", name), { recursive: true });
			await writeFile(
				join(dir, "skills", name, "SKILL.md"),
				`---\nname: ${name}\ndescription: Test skill ${name}\n---\n\nDo ${name} things.\n`,
				"utf-8",
			);
		}
		await fn(dir);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
}

test("recordGenAiCall reports cache tokens alongside input and output", () => {
	exporter.reset();
	recordGenAiCall(FAKE_MSG, { durationMs: 42 });
	const span = exporter.getFinishedSpans().find((s) => s.name === "gen_ai.chat");
	assert.ok(span, "no gen_ai.chat span was exported");
	assert.equal(span!.attributes["gen_ai.usage.input_tokens"], 3);
	assert.equal(span!.attributes["gen_ai.usage.output_tokens"], 179);
	// Without these two the span claims 3 input tokens for a 1523-token call.
	assert.equal(span!.attributes["gen_ai.usage.cache_read_input_tokens"], 11);
	assert.equal(span!.attributes["gen_ai.usage.cache_creation_input_tokens"], 1509);
	assert.equal(span!.attributes["gitagent.cost_usd"], 0.008574);
});

test("recordGenAiCall defaults cache attributes to 0 when the provider omits them", () => {
	exporter.reset();
	recordGenAiCall({ provider: "openai", model: "gpt-4o", usage: { input: 10, output: 20 } });
	const span = exporter.getFinishedSpans().find((s) => s.name === "gen_ai.chat");
	assert.equal(span!.attributes["gen_ai.usage.cache_read_input_tokens"], 0);
	assert.equal(span!.attributes["gen_ai.usage.cache_creation_input_tokens"], 0);
});

test("workflow generation emits a span, and its LLM calls are children of it", async () => {
	await withAgentDir(async (dir) => {
		exporter.reset();
		// Stands in for defaultLlmClient, which reports each assistant message.
		const llm: LlmClient = async () => {
			recordGenAiCall(FAKE_MSG, { durationMs: 10 });
			return VALID_YAML;
		};
		await runGenerate({
			flags: { dir, prompt: "summarize email and post to Slack", dryRun: true, model: "anthropic:claude-sonnet-4-6" },
			llm,
			fitnessLlm: async () => {
				recordGenAiCall(FAKE_MSG, { durationMs: 10 });
				return "[]";
			},
		});

		const spans = exporter.getFinishedSpans();
		const parent = spans.find((s) => s.name === "gitagent.workflow.generate");
		assert.ok(parent, `no workflow span exported, got: ${spans.map((s) => s.name).join(", ")}`);
		assert.equal(parent!.attributes["gitagent.workflow.model"], "anthropic:claude-sonnet-4-6");
		assert.equal(parent!.attributes["gitagent.workflow.attempts"], 1);
		assert.equal(parent!.attributes["gitagent.workflow.fitness_warnings"], 0);
		assert.equal(parent!.attributes["gitagent.workflow.skills_installed"], 2);

		// Both the generation call and the fitness call must hang off the same
		// parent, so one run's cost is one subtree rather than orphan traces.
		const calls = spans.filter((s) => s.name === "gen_ai.chat");
		assert.equal(calls.length, 2, "expected one generation call and one fitness call");
		for (const call of calls) {
			assert.equal(call.parentSpanContext?.spanId, parent!.spanContext().spanId);
			assert.equal(call.spanContext().traceId, parent!.spanContext().traceId);
		}
	});
});

test("a failed generation marks the workflow span as an error", async () => {
	await withAgentDir(async (dir) => {
		exporter.reset();
		const llm: LlmClient = async () => "unsupported:\n  - send a text message\n";
		await assert.rejects(() => runGenerate({ flags: { dir, prompt: "text me", dryRun: true }, llm }));
		const parent = exporter.getFinishedSpans().find((s) => s.name === "gitagent.workflow.generate");
		assert.ok(parent);
		assert.equal(parent!.status.code, 2 /* SpanStatusCode.ERROR */);
		assert.match(String(parent!.status.message), /No installed skill covers/);
	});
});
