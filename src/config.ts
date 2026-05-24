import { readFile } from "fs/promises";
import { join } from "path";
import yaml from "js-yaml";

export interface EnvConfig {
	log_level?: string;
	model_override?: string;
	[key: string]: any;
}

function deepMerge(base: Record<string, any>, override: Record<string, any>): Record<string, any> {
	const result = { ...base };
	for (const key of Object.keys(override)) {
		if (
			result[key] &&
			typeof result[key] === "object" &&
			!Array.isArray(result[key]) &&
			typeof override[key] === "object" &&
			!Array.isArray(override[key])
		) {
			result[key] = deepMerge(result[key], override[key]);
		} else {
			result[key] = override[key];
		}
	}
	return result;
}

async function loadYamlFile(path: string): Promise<Record<string, any>> {
	try {
		const raw = await readFile(path, "utf-8");
		return (yaml.load(raw) as Record<string, any>) || {};
	} catch {
		return {};
	}
}

/**
 * Load environment configuration.
 * Loads config/default.yaml, then merges config/<env>.yaml on top.
 * Env is determined by --env flag or GITAGENT_ENV environment variable.
 */
export async function loadEnvConfig(agentDir: string, env?: string): Promise<EnvConfig> {
	const configDir = join(agentDir, "config");
	const envName = env || process.env.GITAGENT_ENV;

	const base = await loadYamlFile(join(configDir, "default.yaml"));

	if (envName) {
		const envOverride = await loadYamlFile(join(configDir, `${envName}.yaml`));
		return deepMerge(base, envOverride) as EnvConfig;
	}

	return base as EnvConfig;
}
