/**
 * Tests for the telemetry module (src/telemetry.ts) — init/shutdown/idempotency.
 *
 * These tests verify that initTelemetry correctly gates on the OTLP
 * endpoint environment variable: it MUST return without enabling telemetry
 * when no endpoint is configured, and it MUST successfully create an SDK
 * instance when an endpoint (or test provider) is provided.
 */
import { describe, it, before, afterEach } from "node:test";
import assert from "node:assert/strict";
import { trace } from "@opentelemetry/api";
import {
	NodeTracerProvider,
	InMemorySpanExporter,
	SimpleSpanProcessor,
} from "@opentelemetry/sdk-trace-node";

let initTelemetry: typeof import("../src/telemetry.ts").initTelemetry;
let shutdownTelemetry: typeof import("../src/telemetry.ts").shutdownTelemetry;
let isTelemetryEnabled: typeof import("../src/telemetry.ts").isTelemetryEnabled;

before(async () => {
	const mod = await import("../src/telemetry.ts");
	initTelemetry = mod.initTelemetry;
	shutdownTelemetry = mod.shutdownTelemetry;
	isTelemetryEnabled = mod.isTelemetryEnabled;
});

afterEach(async () => {
	await shutdownTelemetry();
	try {
		trace.disable();
	} catch {
		/* ignore */
	}
});

describe("telemetry init", () => {
	function makeTestProvider() {
		const exporter = new InMemorySpanExporter();
		const provider = new NodeTracerProvider({
			spanProcessors: [new SimpleSpanProcessor(exporter)],
		});
		return { exporter, provider };
	}

	it("returns without enabling telemetry when no OTLP endpoint is configured", async () => {
		const saved = process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
		const wasSet = "OTEL_EXPORTER_OTLP_ENDPOINT" in process.env;
		delete process.env.OTEL_EXPORTER_OTLP_ENDPOINT;

		try {
			await assert.doesNotReject(
				() => initTelemetry({}),
				"initTelemetry must never throw, even without an endpoint",
			);

			assert.equal(
				isTelemetryEnabled(),
				false,
				"telemetry must remain disabled when no endpoint is configured",
			);
		} finally {
			if (wasSet) {
				process.env.OTEL_EXPORTER_OTLP_ENDPOINT = saved;
			} else {
				delete process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
			}
		}
	});

	it("creates an SDK instance when endpoint is configured", async () => {
		process.env.OTEL_EXPORTER_OTLP_ENDPOINT = "http://localhost:4318";
		const { exporter, provider } = makeTestProvider();

		try {
			await initTelemetry({
				serviceName: "test-svc",
				_testProvider: provider,
			});

			assert.equal(
				isTelemetryEnabled(),
				true,
				"telemetry must be enabled after initTelemetry with _testProvider",
			);

			const tracer = trace.getTracer("test");
			const span = tracer.startSpan("test-span");
			span.end();

			await provider.forceFlush();
			const spans = exporter.getFinishedSpans();
			assert.equal(spans.length, 1, "span should be exported");
			assert.equal(spans[0].name, "test-span");
		} finally {
			delete process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
		}
	});

	it("is idempotent", async () => {
		const { provider: provider1 } = makeTestProvider();
		const { provider: provider2 } = makeTestProvider();

		await initTelemetry({ _testProvider: provider1 });
		assert.equal(isTelemetryEnabled(), true);

		await initTelemetry({ _testProvider: provider2 });
		assert.equal(isTelemetryEnabled(), true);
	});

	it("shutdownTelemetry resets the initialized state", async () => {
		const { provider } = makeTestProvider();

		await initTelemetry({ _testProvider: provider });
		assert.equal(isTelemetryEnabled(), true);

		await shutdownTelemetry();
		assert.equal(isTelemetryEnabled(), false);
	});
});
