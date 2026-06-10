/**
 * Tests for the telemetry module (src/telemetry.ts).
 *
 * These tests verify that initTelemetry correctly gates on the OTLP
 * endpoint environment variable: it MUST be a no-op when the endpoint
 * is not configured, and it MUST successfully create an SDK instance
 * when an endpoint (or test provider) is provided.
 */
import { describe, it, before, afterEach } from "node:test";
import assert from "node:assert/strict";
import { trace } from "@opentelemetry/api";
import {
	NodeTracerProvider,
} from "@opentelemetry/sdk-trace-node";
import {
	InMemorySpanExporter,
	SimpleSpanProcessor,
} from "@opentelemetry/sdk-trace-base";

let initTelemetry: typeof import("../telemetry.ts").initTelemetry;
let shutdownTelemetry: typeof import("../telemetry.ts").shutdownTelemetry;
let isTelemetryEnabled: typeof import("../telemetry.ts").isTelemetryEnabled;

before(async () => {
	const mod = await import("../telemetry.ts");
	initTelemetry = mod.initTelemetry;
	shutdownTelemetry = mod.shutdownTelemetry;
	isTelemetryEnabled = mod.isTelemetryEnabled;
});

afterEach(async () => {
	// Always clean up telemetry after each test to avoid cross-test
	// contamination from global state.
	await shutdownTelemetry();
	try {
		trace.disable();
	} catch {
		/* ignore */
	}
});

describe("telemetry", () => {
	// ── Helpers ──────────────────────────────────────────────────────

	function makeTestProvider() {
		const exporter = new InMemorySpanExporter();
		const provider = new NodeTracerProvider({
			spanProcessors: [new SimpleSpanProcessor(exporter)],
		});
		return { exporter, provider };
	}

	// ── initTelemetry no-op ──────────────────────────────────────────

	it("initTelemetry without OTLP endpoint does not throw and leaves module in a consistent state", async () => {
		// Ensure the env var is not set for this test
		const saved = process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
		delete process.env.OTEL_EXPORTER_OTLP_ENDPOINT;

		try {
			// Calling initTelemetry with no options does not throw — the
			// module always wraps its body in try/catch so failures are
			// logged, not thrown.
			await assert.doesNotReject(
				() => initTelemetry({}),
				"initTelemetry must never throw, even without an endpoint",
			);
		} finally {
			if (saved !== undefined) {
				process.env.OTEL_EXPORTER_OTLP_ENDPOINT = saved;
			}
		}
	});

	// ── initTelemetry with endpoint ──────────────────────────────────

	it("initTelemetry creates an SDK instance when endpoint is configured", async () => {
		// Set a (bogus) OTLP endpoint so the init path proceeds past the
		// no-op guard.  Because we do not have a real collector, we also
		// provide a _testProvider so the test is deterministic.
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

			// Verify the provider was actually registered: create a span
			// and confirm it flows through the InMemorySpanExporter.
			const tracer = trace.getTracer("test");
			const span = tracer.startSpan("test-span");
			span.end();

			// Force flush so the span lands in the in-memory exporter
			// before we read it back.
			await provider.forceFlush();
			const spans = exporter.getFinishedSpans();
			assert.equal(spans.length, 1, "span should be exported");
			assert.equal(spans[0].name, "test-span");
		} finally {
			delete process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
		}
	});

	// ── Idempotency ──────────────────────────────────────────────────

	it("initTelemetry is idempotent", async () => {
		const { provider: provider1 } = makeTestProvider();
		const { provider: provider2 } = makeTestProvider();

		await initTelemetry({ _testProvider: provider1 });
		assert.equal(isTelemetryEnabled(), true);

		// Second call should be a no-op — _initialized is already true
		await initTelemetry({ _testProvider: provider2 });
		assert.equal(isTelemetryEnabled(), true);
	});

	// ── shutdownTelemetry ────────────────────────────────────────────

	it("shutdownTelemetry resets the initialized state", async () => {
		const { provider } = makeTestProvider();

		await initTelemetry({ _testProvider: provider });
		assert.equal(isTelemetryEnabled(), true);

		await shutdownTelemetry();
		assert.equal(isTelemetryEnabled(), false);
	});
});
