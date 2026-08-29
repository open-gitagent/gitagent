// Zero-config model auto-detection (issue #14).
//
// When no model is configured via --model, agent.yaml, or env config override,
// check known provider API keys (via pi-ai's getEnvApiKey, so Bedrock / Vertex /
// Azure / Copilot ambient credentials are picked up too, not just a hardcoded
// env var list) and fall back to a sensible default model for the first
// provider that has credentials available.

import { getEnvApiKey } from "@mariozechner/pi-ai";

// Checked in this order; first provider with credentials available wins.
// Versionless aliases only, so this never goes stale as new point releases ship.
//
// Direct API-key providers first (explicit, deliberate signal); ambient/
// enterprise credential sources (Bedrock, Vertex, Azure, Copilot) checked
// after, since those can be present incidentally (e.g. a leftover AWS_PROFILE
// or an active gcloud login unrelated to this agent).
const DEFAULT_MODEL_BY_PROVIDER: Array<{ provider: string; model: string }> = [
	{ provider: "anthropic", model: "claude-sonnet-4-6" },
	{ provider: "openai", model: "gpt-4o" },
	{ provider: "google", model: "gemini-2.0-flash" },
	{ provider: "xai", model: "grok-4-fast" },
	{ provider: "groq", model: "llama-3.3-70b-versatile" },
	{ provider: "mistral", model: "mistral-large-latest" },
	{ provider: "amazon-bedrock", model: "anthropic.claude-sonnet-4-6" },
	{ provider: "google-vertex", model: "gemini-2.0-flash" },
	{ provider: "azure-openai-responses", model: "gpt-4o" },
	{ provider: "github-copilot", model: "claude-sonnet-4.6" },
];

export interface AutoDetectedModel {
	modelStr: string;
	provider: string;
}

/**
 * Find the first provider with a usable API key/credential in the environment
 * and return its default model, or undefined if none are configured.
 */
export function autoDetectModel(): AutoDetectedModel | undefined {
	for (const { provider, model } of DEFAULT_MODEL_BY_PROVIDER) {
		if (getEnvApiKey(provider)) {
			return { modelStr: `${provider}:${model}`, provider };
		}
	}
	return undefined;
}
