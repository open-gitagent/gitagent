import { app } from "electron";
import { join } from "path";
import { homedir } from "os";
import { readFileSync, writeFileSync, existsSync, mkdirSync } from "fs";
import type { AppSettings } from "../shared/types";

const DEFAULTS: AppSettings = { model: "openai:gpt-4o-mini", permissionMode: "plan" };

function settingsPath(): string {
	return join(app.getPath("userData"), "settings.json");
}

export function getSettings(): AppSettings {
	let stored: Partial<AppSettings> = {};
	try {
		stored = JSON.parse(readFileSync(settingsPath(), "utf-8"));
	} catch {
		/* first run */
	}
	// Fall back to the gateway URL already loaded from ~/.gitagent/.env at startup.
	const baseUrl = stored.baseUrl ?? process.env.GITAGENT_MODEL_BASE_URL ?? "";
	return { ...DEFAULTS, ...stored, baseUrl };
}

export function saveSettings(s: AppSettings): void {
	// Keys + the gateway URL go to ~/.gitagent/.env (the SDK's global env
	// fallback); everything else is persisted in userData. Never store raw keys
	// in settings.json. The base URL is not secret, so it lives in both.
	const { keys, ...rest } = s;
	writeFileSync(settingsPath(), JSON.stringify(rest, null, 2), "utf-8");

	// GITAGENT_MODEL_BASE_URL is what loader.ts reads to route every model
	// through the gateway; merge it in alongside any API keys.
	const envUpdates: Record<string, string> = { ...(keys ?? {}) };
	if (s.baseUrl) envUpdates.GITAGENT_MODEL_BASE_URL = s.baseUrl;
	if (Object.keys(envUpdates).length > 0) writeEnvKeys(envUpdates);

	// Apply live so the in-process SDK picks up the gateway + key on the next
	// query() without needing an app restart.
	for (const [k, v] of Object.entries(envUpdates)) {
		if (v) process.env[k] = v;
	}
}

/** Merge-write API keys into ~/.gitagent/.env without clobbering other vars. */
function writeEnvKeys(keys: Record<string, string>): void {
	const dir = join(homedir(), ".gitagent");
	if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
	const envPath = join(dir, ".env");
	const existing = existsSync(envPath) ? readFileSync(envPath, "utf-8") : "";

	const map = new Map<string, string>();
	for (const line of existing.split("\n")) {
		const t = line.trim();
		if (!t || t.startsWith("#")) continue;
		const eq = t.indexOf("=");
		if (eq <= 0) continue;
		map.set(t.slice(0, eq).trim(), t.slice(eq + 1));
	}
	for (const [k, v] of Object.entries(keys)) {
		if (v) map.set(k, v);
	}
	const out = [...map.entries()].map(([k, v]) => `${k}=${v}`).join("\n") + "\n";
	writeFileSync(envPath, out, "utf-8");
}
