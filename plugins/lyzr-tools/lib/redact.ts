// Secret redaction for logs and tool results.
//
// Primary defense: values whose *key* looks sensitive are masked. Secondary
// defense: string leaves are also checked by shape against
// TOKEN_SHAPE_RE, since Lyzr's execution API can return a raw OAuth token
// as a plain string under an innocuous key (e.g. `result`) — see the
// "known limitations" note in README.md. The shape check is intentionally
// narrow (known token prefixes, JWTs, and long single-word opaque strings)
// so it doesn't mangle legitimate tool output like email bodies or message
// text — see docs/lyzr-tool-bridge-test-cases.md TC-D03.

const SENSITIVE_KEY_RE =
	/(api[_-]?key|token|secret|password|passwd|credential|authorization|access_token|refresh_token|client_secret|bearer)/i;

const KNOWN_TOKEN_PREFIX_RE = /^(ya29\.|sk-|xox[baprs]-|gh[oprsu]_|AKIA|ASIA|Bearer\s)/;
const JWT_RE = /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/;
const OPAQUE_TOKEN_RE = /^[A-Za-z0-9_.-]{32,}$/;

function looksLikeToken(s: string): boolean {
	if (!s || /\s/.test(s)) return false;
	if (KNOWN_TOKEN_PREFIX_RE.test(s)) return true;
	if (JWT_RE.test(s) && s.length >= 40) return true;
	return OPAQUE_TOKEN_RE.test(s) && /[0-9]/.test(s) && /[A-Za-z]/.test(s);
}

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
	if (typeof value === "string" && looksLikeToken(value)) {
		return maskValue(value);
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
