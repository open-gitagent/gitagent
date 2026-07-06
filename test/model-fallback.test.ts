import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

let isRetryableProviderError: typeof import("../dist/model-fallback.js").isRetryableProviderError;
let loadAgent: typeof import("../dist/exports.js").loadAgent;

before(async () => {
	({ isRetryableProviderError } = await import("../dist/model-fallback.js"));
	({ loadAgent } = await import("../dist/exports.js"));
});

describe("isRetryableProviderError", () => {
	it("retries on billing / credit / quota failures", () => {
		for (const m of [
			"Your credit balance is too low to access the Claude API",
			"insufficient_quota: You exceeded your current quota",
			"billing hard limit reached",
			"429 Too Many Requests",
			"rate limit exceeded",
		]) {
			assert.equal(isRetryableProviderError(m), true, m);
		}
	});

	it("retries on availability / auth failures", () => {
		for (const m of [
			"Overloaded",
			"503 Service Unavailable",
			"internal server error",
			"401 Unauthorized: invalid api key",
			"ETIMEDOUT",
		]) {
			assert.equal(isRetryableProviderError(m), true, m);
		}
	});

	it("retries on empty/missing errors (give the next model a chance)", () => {
		assert.equal(isRetryableProviderError(undefined), true);
		assert.equal(isRetryableProviderError(null), true);
		assert.equal(isRetryableProviderError(""), true);
	});

	it("does NOT retry on request-shaped / unrecognized errors", () => {
		// A malformed-request error won't be fixed by switching models.
		assert.equal(isRetryableProviderError("max_tokens: must be less than 4096"), false);
		assert.equal(isRetryableProviderError("content was blocked by safety filter"), false);
		assert.equal(isRetryableProviderError("something exploded"), false);
	});
});

describe("loadAgent fallback resolution", () => {
	let dir: string;

	before(async () => {
		dir = await mkdtemp(join(tmpdir(), "gitagent-fb-"));
		// Use custom-endpoint models (provider:id@url) so resolution never hits
		// the pi-ai registry and stays deterministic offline.
		const yaml = [
			"spec_version: '1.0'",
			"name: fb-test",
			"version: '1.0.0'",
			"description: fallback test agent",
			"model:",
			"  preferred: 'openai:gpt-4o-mini@https://example.com/v1'",
			"  fallback:",
			"    - 'anthropic:claude-3-5-sonnet@https://example.com/v1'",
			"    - 'openai:gpt-4o-mini@https://example.com/v1'", // dup of preferred → skipped
			"    - ''", // empty → skipped
			"tools: []",
			"runtime:",
			"  max_turns: 10",
			"",
		].join("\n");
		await writeFile(join(dir, "agent.yaml"), yaml, "utf-8");
	});

	it("resolves manifest fallback models, skipping dups and blanks", async () => {
		const loaded = await loadAgent(dir);
		assert.equal(loaded.model.id, "gpt-4o-mini");
		// Only the anthropic entry survives: the openai dup matches preferred and
		// the empty string is skipped.
		assert.equal(loaded.fallbackModels.length, 1);
		assert.equal(loaded.fallbackModels[0].id, "claude-3-5-sonnet");
	});

	it("returns an empty fallback list when none are configured", async () => {
		const d2 = await mkdtemp(join(tmpdir(), "gitagent-fb2-"));
		const yaml = [
			"spec_version: '1.0'",
			"name: no-fb",
			"version: '1.0.0'",
			"description: no fallback",
			"model:",
			"  preferred: 'openai:gpt-4o-mini@https://example.com/v1'",
			"tools: []",
			"runtime:",
			"  max_turns: 10",
			"",
		].join("\n");
		await writeFile(join(d2, "agent.yaml"), yaml, "utf-8");
		const loaded = await loadAgent(d2);
		assert.deepEqual(loaded.fallbackModels, []);
		await rm(d2, { recursive: true, force: true });
	});
});
