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

// Loader (escape hatch)
export { loadAgent } from "./loader.js";
