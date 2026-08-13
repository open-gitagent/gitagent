// Resolve the plugin's raw config (from plugin.yaml defaults / env vars /
// agent.yaml overrides, already merged by gitagent's plugin loader) into a
// strongly-typed ResolvedConfig.

import type { ResolvedConfig } from "./types.ts";

const DEFAULT_BASE_URL = "https://agent-prod.studio.lyzr.ai";
const DEFAULT_TIMEOUT_MS = 10_000;

export function resolveConfig(raw: Record<string, any> | undefined): ResolvedConfig {
	const cfg = raw ?? {};

	const timeoutMs = Number(cfg.timeout_ms);

	return {
		apiKey: String(cfg.api_key ?? "").trim(),
		baseUrl: String(cfg.base_url || DEFAULT_BASE_URL).replace(/\/+$/, ""),
		agentId: cfg.agent_id ? String(cfg.agent_id) : undefined,
		userId: cfg.user_id ? String(cfg.user_id) : undefined,
		workspaceId: cfg.workspace_id ? String(cfg.workspace_id) : undefined,
		includeMcp: cfg.include_mcp !== false,
		preferLyzrTools: cfg.prefer_lyzr_tools !== false,
		timeoutMs: Number.isFinite(timeoutMs) && timeoutMs > 0 ? timeoutMs : DEFAULT_TIMEOUT_MS,
	};
}
