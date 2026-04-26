// OpenTelemetry instrumentation for gitclaw.
//
// Design:
//  - `@opentelemetry/api` is the only hard dependency; it is a no-op when no
//    SDK has been registered as the global tracer/meter provider.
//  - SDK packages (@opentelemetry/sdk-node, exporters, instrumentations, …)
//    are optional peer deps loaded via dynamic `import()` inside
//    `initTelemetry()`. If they are not installed or fail to load, the rest
//    of gitclaw runs unmodified.
//  - Every public function wraps its body in try/catch — telemetry must never
//    crash the agent.
//  - Spans never carry prompt or completion content; only metadata.

import {
	trace,
	metrics,
	context as otelContext,
	SpanStatusCode,
	SpanKind,
} from "@opentelemetry/api";
import type {
	Span,
	Tracer,
	Meter,
	Context,
	Histogram,
	Counter,
} from "@opentelemetry/api";
import type { AgentTool } from "@mariozechner/pi-agent-core";

// ── Public types ───────────────────────────────────────────────────────

export interface TelemetryOptions {
	/** Service name reported as `service.name` resource attribute. */
	serviceName: string;
	/** Optional service version reported as `service.version`. */
	serviceVersion?: string;
	/** OTLP/HTTP endpoint (e.g. `http://localhost:4318`). Reads `OTEL_EXPORTER_OTLP_ENDPOINT` if omitted. */
	exporterEndpoint?: string;
	/** OTLP headers, e.g. `{ Authorization: "Bearer …" }`. */
	headers?: Record<string, string>;
	/** Extra resource attributes to merge into the default resource. */
	resourceAttributes?: Record<string, string | number | boolean>;
	/** Set to `false` to skip metric exporter setup. */
	enableMetrics?: boolean;
	/**
	 * Test escape hatch — register the given TracerProvider directly and
	 * skip all dynamic SDK imports. Used by unit tests.
	 */
	_testProvider?: unknown;
}

// ── Module state ───────────────────────────────────────────────────────

let _initialized = false;
let _sdk: any = null;

const TRACER_NAME = "gitclaw";
const METER_NAME = "gitclaw";

// Lazily-cached metric handles. Created on first use; rely on a no-op meter
// when telemetry is disabled.
const _slots = {
	toolCalls: { v: null as Counter | null },
	toolDuration: { v: null as Histogram | null },
	sessionDuration: { v: null as Histogram | null },
	sessionCost: { v: null as Counter | null },
	genAiToken: { v: null as Counter | null },
	genAiDuration: { v: null as Histogram | null },
};

// ── Initialization ─────────────────────────────────────────────────────

export async function initTelemetry(opts: TelemetryOptions): Promise<void> {
	if (_initialized) return;

	try {
		// Test path — register a caller-supplied TracerProvider directly.
		if (opts._testProvider) {
			const provider = opts._testProvider as {
				register?: () => void;
			};
			if (typeof provider.register === "function") {
				provider.register();
			} else {
				// Fall back to setGlobalTracerProvider for providers without register()
				trace.setGlobalTracerProvider(opts._testProvider as any);
			}
			_initialized = true;
			return;
		}

		// Dynamic imports — keep SDK out of the cold-start path when disabled.
		const sdkNodeMod = await import("@opentelemetry/sdk-node");
		const resourcesMod = await import("@opentelemetry/resources");
		const semconvMod = await import("@opentelemetry/semantic-conventions");
		const traceExporterMod = await import(
			"@opentelemetry/exporter-trace-otlp-http"
		);
		const undiciInstrumentationMod = await import(
			"@opentelemetry/instrumentation-undici"
		);

		const { NodeSDK } = sdkNodeMod;
		const { resourceFromAttributes } = resourcesMod as any;
		const { ATTR_SERVICE_NAME, ATTR_SERVICE_VERSION } = semconvMod as any;
		const { OTLPTraceExporter } = traceExporterMod;
		const { UndiciInstrumentation } = undiciInstrumentationMod;

		const resourceAttrs: Record<string, any> = {
			[ATTR_SERVICE_NAME ?? "service.name"]: opts.serviceName,
			...(opts.resourceAttributes ?? {}),
		};
		if (opts.serviceVersion) {
			resourceAttrs[ATTR_SERVICE_VERSION ?? "service.version"] = opts.serviceVersion;
		}

		const base = opts.exporterEndpoint
			? opts.exporterEndpoint.replace(/\/$/, "")
			: undefined;

		const traceExporter = new OTLPTraceExporter({
			url: base ? `${base}/v1/traces` : undefined,
			headers: opts.headers,
		});

		const sdkConfig: any = {
			resource: resourceFromAttributes(resourceAttrs),
			traceExporter,
			instrumentations: [new UndiciInstrumentation()],
		};

		if (opts.enableMetrics !== false) {
			try {
				const metricsExporterMod = await import(
					"@opentelemetry/exporter-metrics-otlp-http"
				);
				const sdkMetricsMod = await import("@opentelemetry/sdk-metrics");
				const { OTLPMetricExporter } = metricsExporterMod;
				const { PeriodicExportingMetricReader } = sdkMetricsMod;

				const metricExporter = new OTLPMetricExporter({
					url: base ? `${base}/v1/metrics` : undefined,
					headers: opts.headers,
				});
				sdkConfig.metricReader = new PeriodicExportingMetricReader({
					exporter: metricExporter,
				});
			} catch {
				// Metrics packages not installed — continue with traces only.
			}
		}

		_sdk = new NodeSDK(sdkConfig);
		_sdk.start();
		_initialized = true;
	} catch (err) {
		// Never let telemetry init crash the host process. Surface to stderr
		// so misconfiguration is visible without breaking the agent.
		_sdk = null;
		_initialized = false;
		try {
			console.error(
				`[telemetry] init failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		} catch {
			/* ok */
		}
	}
}

export async function shutdownTelemetry(): Promise<void> {
	if (!_initialized) return;
	try {
		if (_sdk) await _sdk.shutdown();
	} catch {
		/* ok */
	} finally {
		_initialized = false;
		_sdk = null;
	}
}

export function isTelemetryEnabled(): boolean {
	return _initialized;
}

// ── Tracer / meter accessors ───────────────────────────────────────────

export function getTracer(): Tracer {
	return trace.getTracer(TRACER_NAME);
}

export function getMeter(): Meter {
	return metrics.getMeter(METER_NAME);
}

function lazyCounter(
	slot: { v: Counter | null },
	name: string,
	description: string,
): Counter {
	if (!slot.v) {
		slot.v = getMeter().createCounter(name, { description });
	}
	return slot.v;
}

function lazyHistogram(
	slot: { v: Histogram | null },
	name: string,
	description: string,
	unit?: string,
): Histogram {
	if (!slot.v) {
		slot.v = getMeter().createHistogram(name, {
			description,
			...(unit ? { unit } : {}),
		});
	}
	return slot.v;
}

function getToolCallCounter(): Counter {
	return lazyCounter(_slots.toolCalls, "gitclaw.tool.calls", "Number of tool executions");
}

function getToolDurationHistogram(): Histogram {
	return lazyHistogram(
		_slots.toolDuration,
		"gitclaw.tool.duration_ms",
		"Tool execution duration in milliseconds",
		"ms",
	);
}

function getSessionDurationHistogram(): Histogram {
	return lazyHistogram(
		_slots.sessionDuration,
		"gitclaw.session.duration_ms",
		"Agent session duration in milliseconds",
		"ms",
	);
}

function getSessionCostCounter(): Counter {
	return lazyCounter(
		_slots.sessionCost,
		"gitclaw.session.cost_usd",
		"Cumulative agent session cost in USD",
	);
}

function getGenAiTokenCounter(): Counter {
	return lazyCounter(
		_slots.genAiToken,
		"gen_ai.client.token.usage",
		"Token usage by GenAI calls",
	);
}

function getGenAiDurationHistogram(): Histogram {
	return lazyHistogram(
		_slots.genAiDuration,
		"gen_ai.client.operation.duration",
		"GenAI operation duration in milliseconds",
		"ms",
	);
}

// ── Session span ───────────────────────────────────────────────────────

export interface SessionHandle {
	span: Span;
	ctx: Context;
	end(extraAttrs?: Record<string, any>): void;
}

export function startSessionSpan(
	name = "gitclaw.agent.session",
	attrs: Record<string, any> = {},
): SessionHandle {
	const startedAt = Date.now();
	let span: Span;
	let ctx: Context;
	try {
		span = getTracer().startSpan(name, {
			kind: SpanKind.INTERNAL,
			attributes: attrs,
		});
		ctx = trace.setSpan(otelContext.active(), span);
	} catch {
		// No-op handle if anything explodes.
		return {
			span: undefined as unknown as Span,
			ctx: otelContext.active(),
			end: () => {},
		};
	}

	let _ended = false;
	return {
		span,
		ctx,
		end(extraAttrs?: Record<string, any>) {
			if (_ended) return;
			_ended = true;
			const durationMs = Date.now() - startedAt;
			try {
				if (extraAttrs) span.setAttributes(extraAttrs);
				span.setAttribute("gitclaw.session.duration_ms", durationMs);
				span.end();
			} catch {
				/* ignore */
			}
			try {
				getSessionDurationHistogram().record(durationMs, {
					"gitclaw.entry": String(attrs["gitclaw.entry"] ?? "unknown"),
				});
				const cost = Number(extraAttrs?.["gitclaw.cost_usd"] ?? 0);
				if (Number.isFinite(cost) && cost > 0) {
					getSessionCostCounter().add(cost, {
						"gitclaw.entry": String(
							attrs["gitclaw.entry"] ?? "unknown",
						),
					});
				}
			} catch {
				/* ignore */
			}
		},
	};
}

// ── Tool wrapper ───────────────────────────────────────────────────────

export function wrapToolWithOtel<T extends AgentTool<any>>(tool: T): T {
	if (!_initialized) return tool;

	const original = (tool as any).execute;
	if (typeof original !== "function") return tool;

	const wrapped = async function (this: any, args: any, ...rest: any[]) {
		const tracer = getTracer();
		const startedAt = Date.now();
		const callId =
			(rest && rest[0] && (rest[0] as any).toolCallId) ||
			`call_${Math.random().toString(36).slice(2, 10)}`;

		return await tracer.startActiveSpan(
			"gitclaw.tool.execute",
			{
				kind: SpanKind.INTERNAL,
				attributes: {
					"tool.name": tool.name,
					"tool.call_id": String(callId),
				},
			},
			async (span) => {
				try {
					const result = await original.apply(this, [args, ...rest]);
					try {
						span.setAttribute("tool.status", "ok");
						span.setStatus({ code: SpanStatusCode.OK });
					} catch {
						/* ignore */
					}
					return result;
				} catch (err) {
					try {
						const message = (err as Error)?.message ?? String(err);
						span.setAttribute("tool.status", "error");
						span.setAttribute("tool.error_message", message);
						span.setStatus({
							code: SpanStatusCode.ERROR,
							message,
						});
					} catch {
						/* ignore */
					}
					throw err;
				} finally {
					const durationMs = Date.now() - startedAt;
					try {
						span.end();
					} catch {
						/* ignore */
					}
					try {
						getToolCallCounter().add(1, { "tool.name": tool.name });
						getToolDurationHistogram().record(durationMs, {
							"tool.name": tool.name,
						});
					} catch {
						/* ignore */
					}
				}
			},
		);
	};

	// Preserve all other tool fields (name, description, schema, …) and
	// override only execute.
	return new Proxy(tool as any, {
		get(target, prop, receiver) {
			if (prop === "execute") return wrapped;
			return Reflect.get(target, prop, receiver);
		},
	}) as T;
}

// ── gen_ai.chat span ───────────────────────────────────────────────────

export interface RecordGenAiOptions {
	durationMs?: number;
}

export function recordGenAiCall(
	msg: any,
	opts: RecordGenAiOptions = {},
): void {
	if (!_initialized) return;
	if (!msg) return;

	try {
		const system = String(msg.provider ?? msg.api ?? "unknown");
		const model = String(msg.model ?? "unknown");
		const inputTokens = Number(
			msg.usage?.input ?? msg.usage?.inputTokens ?? 0,
		);
		const outputTokens = Number(
			msg.usage?.output ?? msg.usage?.outputTokens ?? 0,
		);
		const cost = Number(
			msg.usage?.cost?.total ?? msg.usage?.cost ?? 0,
		);
		const finishReason = msg.stopReason ?? msg.stop_reason ?? "unknown";

		const span = getTracer().startSpan("gen_ai.chat", {
			kind: SpanKind.CLIENT,
			attributes: {
				"gen_ai.system": system,
				"gen_ai.request.model": model,
				"gen_ai.response.finish_reasons": [String(finishReason)],
				"gen_ai.usage.input_tokens": inputTokens,
				"gen_ai.usage.output_tokens": outputTokens,
				"gitclaw.cost_usd": Number.isFinite(cost) ? cost : 0,
			},
		});

		if (msg.stopReason === "error") {
			span.setStatus({
				code: SpanStatusCode.ERROR,
				message: typeof msg.errorMessage === "string"
					? msg.errorMessage.slice(0, 200)
					: "llm_error",
			});
		}
		span.end();

		try {
			const tokenCounter = getGenAiTokenCounter();
			if (inputTokens > 0) {
				tokenCounter.add(inputTokens, {
					"gen_ai.system": system,
					"gen_ai.request.model": model,
					"gen_ai.token.type": "input",
				});
			}
			if (outputTokens > 0) {
				tokenCounter.add(outputTokens, {
					"gen_ai.system": system,
					"gen_ai.request.model": model,
					"gen_ai.token.type": "output",
				});
			}
			if (typeof opts.durationMs === "number" && opts.durationMs >= 0) {
				getGenAiDurationHistogram().record(opts.durationMs, {
					"gen_ai.system": system,
					"gen_ai.request.model": model,
				});
			}
		} catch {
			/* ignore */
		}
	} catch {
		/* swallow — telemetry must never throw */
	}
}
