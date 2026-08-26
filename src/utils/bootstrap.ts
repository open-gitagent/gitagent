// Start-up context derived from an agent directory: .env files, telemetry
// auto-init, and the manifest's preferred model.
//
// These three steps used to live inline in main(), which meant any command that
// returned before reaching them silently ran without them — that is how
// `gitagent workflow generate` ended up with no telemetry and no agent.yaml
// model. Keeping them here lets every entry point share one implementation.

import { existsSync, readFileSync } from "fs";
import { readFile } from "fs/promises";
import { homedir } from "os";
import { join, resolve } from "path";
import yaml from "js-yaml";
import { initTelemetry } from "../telemetry.js";

function loadEnvPath(envPath: string): void {
	if (!existsSync(envPath)) return;
	const envContent = readFileSync(envPath, "utf-8");
	for (const rawLine of envContent.split("\n")) {
		const line = rawLine.trim();
		if (!line || line.startsWith("#")) continue;
		const eq = line.indexOf("=");
		if (eq <= 0) continue;
		const key = line.slice(0, eq).trim();
		let val = line.slice(eq + 1).trim();
		if ((val.startsWith('"') && val.endsWith('"')) || (val.startsWith("'") && val.endsWith("'"))) {
			val = val.slice(1, -1);
		}
		process.env[key] = val;
	}
}

/**
 * Env precedence (lowest → highest): inherited env → ~/.gitagent/.env (global
 * fallback) → agent-dir .env (winner). Sources are applied in that order so the
 * last one wins; an agent's .env still beats a shell placeholder.
 */
export function loadDotEnvFiles(agentDir: string): void {
	loadEnvPath(join(homedir(), ".gitagent", ".env"));
	loadEnvPath(resolve(agentDir, ".env"));
}

/**
 * Initialize telemetry when the OTEL env vars ask for it. Must run after
 * loadDotEnvFiles() so OTEL_* values set in a .env are picked up.
 * Returns whether initialization was attempted.
 */
export async function maybeInitTelemetry(): Promise<boolean> {
	const wanted =
		(process.env.OTEL_EXPORTER_OTLP_ENDPOINT || process.env.OTEL_TRACES_EXPORTER === "console") &&
		process.env.GITAGENT_OTEL_ENABLED !== "false";
	if (!wanted) return false;
	await initTelemetry({});
	return true;
}

/**
 * Reads `model.preferred` from the agent's manifest. Returns undefined when
 * there is no agent.yaml, it cannot be parsed, or the field is absent — a
 * workflow can be generated in a bare directory, so none of those are errors.
 */
export async function readPreferredModel(agentDir: string): Promise<string | undefined> {
	let raw: string;
	try {
		raw = await readFile(join(agentDir, "agent.yaml"), "utf-8");
	} catch {
		return undefined;
	}
	let parsed: unknown;
	try {
		parsed = yaml.load(raw);
	} catch {
		return undefined;
	}
	const preferred = (parsed as any)?.model?.preferred;
	return typeof preferred === "string" && preferred.trim() ? preferred.trim() : undefined;
}
