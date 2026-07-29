/**
 * Model fallback support.
 *
 * `manifest.model.fallback` lets an agent declare alternate models to use when
 * the primary provider fails. Historically this list was parsed but never used
 * at runtime — a single provider outage (e.g. Anthropic "credit balance too
 * low") would kill the run even when a working provider was also configured
 * (issue #24). This module decides which provider errors are worth retrying on
 * a different model.
 */

// Signatures of provider-side failures where switching to a different model
// (usually a different provider/account) is likely to succeed. Deliberately
// conservative: we only retry on availability/billing/auth-class errors, never
// on errors caused by the request itself (bad input, content filter, etc.).
const RETRYABLE_PATTERNS: RegExp[] = [
	/credit balance/i,
	/insufficient (?:credit|quota|funds|balance)/i,
	/quota/i,
	/billing/i,
	/payment/i,
	/rate.?limit/i,
	/\b429\b/,
	/overloaded/i,
	/\b529\b/,
	/temporarily unavailable/i,
	/service unavailable/i,
	/\b503\b/,
	/\b502\b/,
	/\b500\b/,
	/internal server error/i,
	/timeout/i,
	/timed out/i,
	/econnreset|econnrefused|etimedout|enotfound/i,
	/authentication|unauthorized|invalid api key|invalid x-api-key|permission/i,
	/\b401\b/,
	/\b403\b/,
];

/**
 * Whether a provider error message indicates the request should be retried on
 * the next fallback model.
 *
 * - Empty/missing message → retry: we have no detail, so give the next model a
 *   chance rather than dead-ending on an opaque failure.
 * - Recognized availability/billing/auth pattern → retry.
 * - Any other non-empty message (e.g. a malformed-request error) → do not
 *   retry: switching models won't fix a bad request.
 */
export function isRetryableProviderError(message: string | undefined | null): boolean {
	if (!message) return true;
	return RETRYABLE_PATTERNS.some((re) => re.test(message));
}
