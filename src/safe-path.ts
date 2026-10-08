import { resolve, sep } from "path";

// A single path segment: letters, digits, dot, underscore, dash. No separators,
// no leading dot (rules out "." and ".."), no leading dash.
const SAFE_SEGMENT_RE = /^[A-Za-z0-9_][A-Za-z0-9._-]*$/;

export function isSafePathSegment(name: string): boolean {
	return typeof name === "string" && SAFE_SEGMENT_RE.test(name) && !name.includes("..");
}

/**
 * Resolve `baseDir/<name><suffix>` and guarantee the result is a direct
 * child of `baseDir`. Throws on anything that could escape it.
 */
export function safeChildPath(baseDir: string, name: string, suffix = "", label = "name"): string {
	if (!isSafePathSegment(name)) {
		throw new Error(`Invalid ${label}: ${JSON.stringify(name)}`);
	}
	const base = resolve(baseDir);
	const target = resolve(base, `${name}${suffix}`);
	if (!target.startsWith(base + sep) || target.slice(base.length + 1).includes(sep)) {
		throw new Error(`Invalid ${label}: ${JSON.stringify(name)}`);
	}
	return target;
}
