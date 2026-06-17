/**
 * Shared `${VAR}` environment-variable interpolation.
 *
 * Replaces `${VAR_NAME}` occurrences in strings with `process.env.VAR_NAME`,
 * or an empty string when the variable is unset (matching the long-standing
 * plugin-config behavior). Recurses through arrays and plain objects so an
 * entire config object (e.g. an MCP server definition) can be interpolated in
 * one call. Non-string leaf values are returned unchanged.
 */
const ENV_VAR_PATTERN = /\$\{(\w+)\}/g;

export function interpolateEnvString(value: string): string {
	return value.replace(ENV_VAR_PATTERN, (_, envName) => process.env[envName] || "");
}

export function interpolateEnv<T>(value: T): T {
	if (typeof value === "string") {
		return interpolateEnvString(value) as unknown as T;
	}
	if (Array.isArray(value)) {
		return value.map((item) => interpolateEnv(item)) as unknown as T;
	}
	if (value && typeof value === "object") {
		const out: Record<string, any> = {};
		for (const [key, v] of Object.entries(value as Record<string, any>)) {
			out[key] = interpolateEnv(v);
		}
		return out as unknown as T;
	}
	return value;
}
