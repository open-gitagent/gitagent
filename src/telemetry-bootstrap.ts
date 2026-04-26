// Preload module for `node --import ./dist/telemetry-bootstrap.js …` (or
// `--require` style import maps). Initialises the OpenTelemetry SDK as early
// as possible so HTTP instrumentation patches `undici` before the agent
// makes any LLM calls.
//
// Disable via env: `GITCLAW_OTEL_ENABLED=false`.

import { createRequire } from "module";
import { initTelemetry, shutdownTelemetry } from "./telemetry.js";

const _require = createRequire(import.meta.url);
const _pkg = _require("../package.json") as { version: string };

function parseHeaders(raw?: string): Record<string, string> | undefined {
	if (!raw) return undefined;
	const out: Record<string, string> = {};
	for (const part of raw.split(",")) {
		const eq = part.indexOf("=");
		if (eq <= 0) continue;
		const k = part.slice(0, eq).trim();
		const v = part.slice(eq + 1).trim();
		if (k) out[k] = v;
	}
	return Object.keys(out).length ? out : undefined;
}

if (process.env.GITCLAW_OTEL_ENABLED !== "false") {
	await initTelemetry({
		serviceName: process.env.GITCLAW_OTEL_SERVICE_NAME ?? "gitclaw",
		serviceVersion: process.env.GITCLAW_OTEL_SERVICE_VERSION ?? _pkg.version,
		exporterEndpoint: process.env.OTEL_EXPORTER_OTLP_ENDPOINT,
		headers: parseHeaders(process.env.OTEL_EXPORTER_OTLP_HEADERS),
	});

	const shutdown = (): void => {
		shutdownTelemetry().catch(() => {
			/* ignore */
		});
	};
	process.once("beforeExit", shutdown);
	process.on("SIGTERM", shutdown);
	process.on("SIGINT", shutdown);
}
