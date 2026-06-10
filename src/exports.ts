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

// Voice — startVoiceServer moved to the optional @open-gitagent/voice package
// in v2.0.0. The message-type protocol below stays in core because chat history
// and the standalone scheduler persist and emit these types.

// Multimodal adapter protocol types (web-UI client/server message bus)
export type {
	AdapterBackend,
	ClientMessage,
	ClientAudioMessage,
	ClientVideoFrameMessage,
	ClientTextMessage,
	ClientFileMessage,
	ServerMessage,
	ServerAudioDelta,
	ServerTranscript,
	ServerAgentWorking,
	ServerAgentDone,
	ServerToolCall,
	ServerToolResult,
	ServerAgentThinking,
	ServerError,
	ServerInterrupt,
	ServerFilesChanged,
	ServerMemorySaving,
	ServerLogEntry,
	MultimodalAdapter,
	MultimodalAdapterConfig,
	VoiceServerOptions,
	VoiceAdapter,
	VoiceAdapterConfig,
} from "./adapter.js";
export { DEFAULT_VOICE_INSTRUCTIONS } from "./adapter.js";

// Chat history persistence (used by voice and by any non-voice consumer)
export {
	appendMessage,
	loadHistory,
	deleteHistory,
	summarizeHistory,
} from "./chat-history.js";

// Symbols re-exported for @open-gitagent/voice (consumed via peer dependency).
// These are stable enough for the voice package to depend on; flagged here so
// future renames know they're part of the public-ish surface.
export { getVoiceContext, getAgentContext } from "./context.js";
export { discoverSkills } from "./skills.js";
export {
	discoverWorkflows,
	loadFlowDefinition,
	saveFlowDefinition,
	deleteFlowDefinition,
} from "./workflows.js";
export {
	discoverSchedules,
	saveSchedule,
	deleteSchedule,
	updateScheduleMeta,
} from "./schedules.js";
export {
	startScheduler,
	stopScheduler,
	reloadSchedules,
	executeScheduledJob,
} from "./schedule-runner.js";

// Plugin types
export type { PluginManifest, PluginConfig, LoadedPlugin } from "./plugin-types.js";
export type { GitagentPluginApi } from "./plugin-sdk.js";
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
