// Secret redaction for logs and tool results.
//
// Only values whose *key* looks sensitive are masked. We deliberately do not
// try to pattern-match "secret-looking" strings by shape, because that would
// also mangle legitimate tool output (email bodies, message text, etc.) —
// see docs/lyzr-tool-bridge-test-cases.md TC-D03.

const SENSITIVE_KEY_RE =
	/(api[_-]?key|token|secret|password|passwd|credential|authorization|access_token|refresh_token|client_secret|bearer)/i;

export function redactSecrets<T>(value: T): T {
	return redactValue(value) as T;
}

function redactValue(value: unknown): unknown {
	if (Array.isArray(value)) {
		return value.map((v) => redactValue(v));
	}
	if (value && typeof value === "object") {
		const out: Record<string, unknown> = {};
		for (const [key, v] of Object.entries(value as Record<string, unknown>)) {
			out[key] = SENSITIVE_KEY_RE.test(key) ? maskValue(v) : redactValue(v);
		}
		return out;
	}
	return value;
}

function maskValue(v: unknown): string {
	if (v === null || v === undefined) return "[redacted]";
	const s = typeof v === "string" ? v : JSON.stringify(v);
	if (!s) return "[redacted]";
	if (s.length <= 4) return "****";
	return `${s.slice(0, 2)}${"*".repeat(Math.min(8, s.length - 4))}${s.slice(-2)}`;
}
