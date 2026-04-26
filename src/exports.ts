// SDK core
export { query, tool } from "./sdk.js";

// SDK types
export type {
	Query,
	QueryOptions,
	LocalRepoOptions,
	SandboxOptions,
	GCMessage,
	GCAssistantMessage,
	GCUserMessage,
	GCToolUseMessage,
	GCToolResultMessage,
	GCSystemMessage,
	GCStreamDelta,
	GCToolDefinition,
	GCHooks,
	GCHookResult,
	GCPreToolUseContext,
	GCHookContext,
} from "./sdk-types.js";

// Internal types (for advanced usage)
export type { AgentManifest, LoadedAgent } from "./loader.js";
export type { SkillMetadata } from "./skills.js";
export type { WorkflowMetadata } from "./workflows.js";
export type { SubAgentMetadata } from "./agents.js";
export type { ComplianceWarning } from "./compliance.js";
export type { EnvConfig } from "./config.js";

// Sandbox
export type { SandboxConfig, SandboxContext } from "./sandbox.js";
export { createSandboxContext } from "./sandbox.js";

// Session
export type { LocalSession } from "./session.js";
export { initLocalSession } from "./session.js";

// Voice
export type { VoiceAdapter, VoiceAdapterConfig, VoiceServerOptions } from "./voice/adapter.js";
export { startVoiceServer } from "./voice/server.js";

// Plugin types
export type { PluginManifest, PluginConfig, LoadedPlugin } from "./plugin-types.js";
export type { GitclawPluginApi } from "./plugin-sdk.js";
export { createPluginApi } from "./plugin-sdk.js";

// Tool factory (Claude Code buildTool pattern)
export { buildTool, getToolMetadata } from "./tool-factory.js";
export type { ToolDefinition, ToolMetadata } from "./tool-factory.js";

// Cost tracking
export { CostTracker } from "./cost-tracker.js";
export type { SessionCosts, ModelUsage } from "./cost-tracker.js";

// Context compaction
export { estimateTokens, estimateMessageTokens, needsCompaction, truncateToolResults, messagesToText, buildCompactPrompt } from "./compact.js";

// Loader (escape hatch)
export { loadAgent } from "./loader.js";

// Telemetry (OpenTelemetry instrumentation)
export {
	initTelemetry,
	shutdownTelemetry,
	isTelemetryEnabled,
} from "./telemetry.js";
export type { TelemetryOptions } from "./telemetry.js";
