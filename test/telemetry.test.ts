// Unit tests for src/telemetry.ts
//
// Strategy: register an InMemorySpanExporter behind a NodeTracerProvider as
// the global tracer provider via `_testProvider`. This skips the dynamic SDK
// imports entirely so tests are fast and self-contained.

import test from "node:test";
import assert from "node:assert/strict";
import { trace } from "@opentelemetry/api";
import {
	NodeTracerProvider,
	InMemorySpanExporter,
	SimpleSpanProcessor,
} from "@opentelemetry/sdk-trace-node";

import {
	initTelemetry,
	shutdownTelemetry,
	startSessionSpan,
	wrapToolWithOtel,
	recordGenAiCall,
	isTelemetryEnabled,
} from "../src/telemetry.ts";

// ── Test scaffolding ───────────────────────────────────────────────────

function freshExporter(): {
	exporter: InMemorySpanExporter;
	provider: NodeTracerProvider;
} {
	const exporter = new InMemorySpanExporter();
	const provider = new NodeTracerProvider({
		spanProcessors: [new SimpleSpanProcessor(exporter)],
	});
	return { exporter, provider };
}

async function withTelemetry(
	fn: (exporter: InMemorySpanExporter) => Promise<void> | void,
): Promise<void> {
	const { exporter, provider } = freshExporter();
	await initTelemetry({ serviceName: "gitclaw-test", _testProvider: provider });
	try {
		await fn(exporter);
	} finally {
		await shutdownTelemetry();
		// Reset any global tracer provider side-effect
		try {
			trace.disable();
		} catch {
			/* ignore */
		}
	}
}

// ── Tests ──────────────────────────────────────────────────────────────

test("wrapToolWithOtel happy path produces gitclaw.tool.execute span with status=ok", async () => {
	await withTelemetry(async (exporter) => {
		const tool: any = {
			name: "echo",
			description: "echo",
			parameters: {} as any,
			execute: async (args: any) => `hello ${args.name}`,
		};
		const wrapped = wrapToolWithOtel(tool);
		const result = await (wrapped as any).execute({ name: "world" });
		assert.equal(result, "hello world");

		const spans = exporter.getFinishedSpans();
		const toolSpan = spans.find((s) => s.name === "gitclaw.tool.execute");
		assert.ok(toolSpan, "expected gitclaw.tool.execute span");
		assert.equal(toolSpan!.attributes["tool.name"], "echo");
		assert.equal(toolSpan!.attributes["tool.status"], "ok");
	});
});

test("wrapToolWithOtel error path sets status=error and records error message", async () => {
	await withTelemetry(async (exporter) => {
		const tool: any = {
			name: "boom",
			description: "boom",
			parameters: {} as any,
			execute: async () => {
				throw new Error("kaboom");
			},
		};
		const wrapped = wrapToolWithOtel(tool);
		await assert.rejects(
			() => (wrapped as any).execute({}),
			/kaboom/,
		);
		const spans = exporter.getFinishedSpans();
		const toolSpan = spans.find((s) => s.name === "gitclaw.tool.execute");
		assert.ok(toolSpan);
		assert.equal(toolSpan!.attributes["tool.status"], "error");
		assert.equal(toolSpan!.attributes["tool.error_message"], "kaboom");
		// SpanStatusCode.ERROR === 2
		assert.equal(toolSpan!.status.code, 2);
	});
});

test("startSessionSpan + child tool span produce a parent/child relationship", async () => {
	const { context: otelContext } = await import("@opentelemetry/api");
	await withTelemetry(async (exporter) => {
		const session = startSessionSpan("gitclaw.agent.session", {
			"gitclaw.entry": "test",
		});
		const tool: any = {
			name: "child",
			description: "",
			parameters: {} as any,
			execute: async () => "ok",
		};
		const wrapped = wrapToolWithOtel(tool);
		// Run the tool inside the session's active context — mirrors what
		// sdk.ts/index.ts do via otelContext.with(_session.ctx, agent.prompt).
		await otelContext.with(session.ctx, () => (wrapped as any).execute({}));
		session.end();

		const spans = exporter.getFinishedSpans();
		const parent = spans.find((s) => s.name === "gitclaw.agent.session");
		const child = spans.find((s) => s.name === "gitclaw.tool.execute");
		assert.ok(parent && child);
		assert.equal(parent!.attributes["gitclaw.entry"], "test");
		assert.ok(
			typeof parent!.attributes["gitclaw.session.duration_ms"] === "number",
			"session duration recorded",
		);
		// Strong assertion: child must hang off the session span.
		const childParentId =
			(child as any).parentSpanId ?? (child as any).parentSpanContext?.spanId;
		assert.ok(childParentId, "child span must have a parentSpanId");
		assert.equal(
			childParentId,
			parent!.spanContext().spanId,
			"tool span must be a child of the session span",
		);
	});
});

test("startSessionSpan end() is idempotent — calling twice records only one span", async () => {
	await withTelemetry(async (exporter) => {
		const session = startSessionSpan("gitclaw.agent.session", {
			"gitclaw.entry": "test",
		});
		session.end();
		session.end(); // second call must be a no-op

		const sessions = exporter
			.getFinishedSpans()
			.filter((s) => s.name === "gitclaw.agent.session");
		assert.equal(sessions.length, 1, "session span must appear exactly once");
	});
});

test("recordGenAiCall emits gen_ai.chat span with the documented attributes", async () => {
	await withTelemetry(async (exporter) => {
		recordGenAiCall(
			{
				provider: "openai",
				model: "gpt-4o",
				stopReason: "stop",
				usage: {
					input: 100,
					output: 50,
					cost: { total: 0.0042 },
				},
			},
			{ durationMs: 123 },
		);

		const spans = exporter.getFinishedSpans();
		const span = spans.find((s) => s.name === "gen_ai.chat");
		assert.ok(span);
		assert.equal(span!.attributes["gen_ai.system"], "openai");
		assert.equal(span!.attributes["gen_ai.request.model"], "gpt-4o");
		assert.equal(span!.attributes["gen_ai.usage.input_tokens"], 100);
		assert.equal(span!.attributes["gen_ai.usage.output_tokens"], 50);
		assert.equal(span!.attributes["gitclaw.cost_usd"], 0.0042);
		assert.deepEqual(
			span!.attributes["gen_ai.response.finish_reasons"],
			["stop"],
		);
	});
});

test("recordGenAiCall with stopReason=error sets span status to ERROR", async () => {
	await withTelemetry(async (exporter) => {
		recordGenAiCall(
			{
				provider: "openai",
				model: "gpt-4o",
				stopReason: "error",
				errorMessage: "rate_limit_exceeded",
				usage: { input: 10, output: 0, cost: { total: 0 } },
			},
			{ durationMs: 0 },
		);

		const spans = exporter.getFinishedSpans();
		const span = spans.find((s) => s.name === "gen_ai.chat");
		assert.ok(span, "expected gen_ai.chat span");
		// SpanStatusCode.ERROR === 2
		assert.equal(span!.status.code, 2, "span status must be ERROR");
		assert.ok(
			typeof span!.status.message === "string" && span!.status.message.length > 0,
			"span status message must be set",
		);
	});
});

test("no-ops without init: no spans emitted, no throws", async () => {
	// Make sure prior tests didn't leak initialization
	await shutdownTelemetry();
	assert.equal(isTelemetryEnabled(), false);

	const tool: any = {
		name: "noop",
		description: "",
		parameters: {} as any,
		execute: async () => "still works",
	};
	const wrapped = wrapToolWithOtel(tool);
	// wrapped should be the *same* object since telemetry is disabled
	assert.equal(wrapped, tool);
	const result = await (wrapped as any).execute({});
	assert.equal(result, "still works");

	// recordGenAiCall must not throw
	assert.doesNotThrow(() =>
		recordGenAiCall({ model: "x", provider: "y", usage: {} }),
	);

	// startSessionSpan returns a no-op handle
	const handle = startSessionSpan("gitclaw.agent.session", {
		"gitclaw.entry": "none",
	});
	assert.doesNotThrow(() => handle.end());
});

test("initTelemetry is idempotent — second call is a no-op", async () => {
	const { exporter, provider } = freshExporter();
	await initTelemetry({ serviceName: "gitclaw-test", _testProvider: provider });
	const enabledAfterFirst = isTelemetryEnabled();

	try {
		// Second call with a *different* provider should not register it
		const { provider: provider2 } = freshExporter();
		await initTelemetry({ serviceName: "again", _testProvider: provider2 });

		assert.equal(enabledAfterFirst, true);
		assert.equal(isTelemetryEnabled(), true);

		// Spans should still flow into the original exporter
		const tool: any = {
			name: "idem",
			description: "",
			parameters: {} as any,
			execute: async () => "ok",
		};
		const wrapped = wrapToolWithOtel(tool);
		await (wrapped as any).execute({});

		const spans = exporter.getFinishedSpans();
		assert.ok(spans.find((s) => s.name === "gitclaw.tool.execute"));
	} finally {
		await shutdownTelemetry();
		try {
			trace.disable();
		} catch {
			/* ignore */
		}
	}
});
