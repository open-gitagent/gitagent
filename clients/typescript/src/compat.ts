/**
 * Names the TypeScript gitagent (1.x and 2.x) exported that gitagent 3 does
 * not provide, because the binary now does that work itself. They are kept so
 * that code importing them still loads; calling one throws a `GitagentError`
 * that says what to use instead. See docs/MIGRATING.md.
 */

import { GitagentError, type GCMessage, type JsonObject } from "./index.js";

function removed(name: string, instead: string): (...args: unknown[]) => never {
  return () => {
    throw new GitagentError(`${name} is not part of gitagent 3: ${instead}. See docs/MIGRATING.md.`);
  };
}

const LOADED_BY_BINARY = "the binary reads the agent folder itself; check a folder with `gitagent validate`";
const SESSIONS = "the binary stores sessions; pass `sessionId` and `resume: true` to query()";
const SCHEDULES = "use `gitagent schedule list|add|run|start|enable|disable|remove`";
const CONTEXT = "the binary manages the context (masking and compaction)";
const TELEMETRY = "the binary exports traces and metrics when OTEL_EXPORTER_OTLP_ENDPOINT is set";
const VOICE = "voice mode is not part of gitagent 3";

/** @deprecated Not in gitagent 3: set `sandbox` in gitagent.yaml; the binary sandboxes the agent's shell. */
export const createSandboxContext = removed("createSandboxContext", "set `sandbox` in gitagent.yaml; the binary sandboxes the agent's shell");
/** @deprecated Not in gitagent 3: pass the `repo` option to query(). */
export const initLocalSession = removed("initLocalSession", "pass the `repo` option to query()");
/** @deprecated Not in gitagent 3: voice mode is not part of gitagent 3. */
export const DEFAULT_VOICE_INSTRUCTIONS = "";
/** @deprecated Not in gitagent 3: the binary stores sessions. */
export const appendMessage = removed("appendMessage", SESSIONS);
/** @deprecated Not in gitagent 3: the binary stores sessions. */
export const loadHistory = removed("loadHistory", SESSIONS);
/** @deprecated Not in gitagent 3: the binary stores sessions. */
export const deleteHistory = removed("deleteHistory", SESSIONS);
/** @deprecated Not in gitagent 3: the binary stores sessions. */
export const summarizeHistory = removed("summarizeHistory", SESSIONS);
/** @deprecated Not in gitagent 3: voice mode is not part of gitagent 3. */
export const getVoiceContext = removed("getVoiceContext", VOICE);
/** @deprecated Not in gitagent 3: the binary assembles the system prompt. */
export const getAgentContext = removed("getAgentContext", "the binary assembles the system prompt from the agent folder");
/** @deprecated Not in gitagent 3: the binary reads `skills/`. */
export const discoverSkills = removed("discoverSkills", LOADED_BY_BINARY);
/** @deprecated Not in gitagent 3: the binary reads `workflows/`; run one with `gitagent --workflow`. */
export const discoverWorkflows = removed("discoverWorkflows", LOADED_BY_BINARY);
/** @deprecated Not in gitagent 3: workflows are files in `workflows/`. */
export const loadFlowDefinition = removed("loadFlowDefinition", LOADED_BY_BINARY);
/** @deprecated Not in gitagent 3: workflows are files in `workflows/`. */
export const saveFlowDefinition = removed("saveFlowDefinition", "workflows are files in `workflows/`; write them directly");
/** @deprecated Not in gitagent 3: workflows are files in `workflows/`. */
export const deleteFlowDefinition = removed("deleteFlowDefinition", "workflows are files in `workflows/`; delete them directly");
/** @deprecated Not in gitagent 3: use `gitagent schedule`. */
export const discoverSchedules = removed("discoverSchedules", SCHEDULES);
/** @deprecated Not in gitagent 3: use `gitagent schedule`. */
export const saveSchedule = removed("saveSchedule", SCHEDULES);
/** @deprecated Not in gitagent 3: use `gitagent schedule`. */
export const deleteSchedule = removed("deleteSchedule", SCHEDULES);
/** @deprecated Not in gitagent 3: use `gitagent schedule`. */
export const updateScheduleMeta = removed("updateScheduleMeta", SCHEDULES);
/** @deprecated Not in gitagent 3: use `gitagent schedule start`. */
export const startScheduler = removed("startScheduler", SCHEDULES);
/** @deprecated Not in gitagent 3: use `gitagent schedule`. */
export const stopScheduler = removed("stopScheduler", SCHEDULES);
/** @deprecated Not in gitagent 3: use `gitagent schedule`. */
export const reloadSchedules = removed("reloadSchedules", SCHEDULES);
/** @deprecated Not in gitagent 3: use `gitagent schedule run`. */
export const executeScheduledJob = removed("executeScheduledJob", SCHEDULES);
/** @deprecated Not in gitagent 3: plugins contribute tools, skills, hooks and prompt text through files. */
export const createPluginApi = removed("createPluginApi", "plugins contribute tools, skills, hooks and prompt text through files; see `gitagent plugin`");
/** @deprecated Not in gitagent 3: use tool(). */
export const buildTool = removed("buildTool", "declare tools with tool() or in the agent's tools/ folder");
/** @deprecated Not in gitagent 3: use tool(). */
export const getToolMetadata = removed("getToolMetadata", "declare tools with tool() or in the agent's tools/ folder");
/** @deprecated Not in gitagent 3: the binary manages the context. */
export const estimateTokens = removed("estimateTokens", CONTEXT);
/** @deprecated Not in gitagent 3: the binary manages the context. */
export const estimateMessageTokens = removed("estimateMessageTokens", CONTEXT);
/** @deprecated Not in gitagent 3: the binary manages the context. */
export const needsCompaction = removed("needsCompaction", CONTEXT);
/** @deprecated Not in gitagent 3: the binary manages the context. */
export const truncateToolResults = removed("truncateToolResults", CONTEXT);
/** @deprecated Not in gitagent 3: the binary manages the context. */
export const messagesToText = removed("messagesToText", CONTEXT);
/** @deprecated Not in gitagent 3: the binary manages the context. */
export const buildCompactPrompt = removed("buildCompactPrompt", CONTEXT);
/** @deprecated Not in gitagent 3: the binary reads the agent folder. */
export const loadAgent = removed("loadAgent", LOADED_BY_BINARY);
/** @deprecated Not in gitagent 3: set OTEL_EXPORTER_OTLP_ENDPOINT. */
export const initTelemetry = removed("initTelemetry", TELEMETRY);
/** @deprecated Not in gitagent 3: set OTEL_EXPORTER_OTLP_ENDPOINT. */
export const shutdownTelemetry = removed("shutdownTelemetry", TELEMETRY);
/** @deprecated Not in gitagent 3: set OTEL_EXPORTER_OTLP_ENDPOINT. */
export const isTelemetryEnabled = removed("isTelemetryEnabled", TELEMETRY);

/** @deprecated Not in gitagent 3: use `q.costs()` on the query. */
export class CostTracker {
  constructor() {
    throw new GitagentError("CostTracker is not part of gitagent 3: use q.costs() on the query. See docs/MIGRATING.md.");
  }
}

// Message types of 1.x, all carried by `GCMessage` and told apart by `type`.
/** @deprecated Use `GCMessage`. */ export type GCAssistantMessage = GCMessage;
/** @deprecated Use `GCMessage`. */ export type GCToolUseMessage = GCMessage;
/** @deprecated Use `GCMessage`. */ export type GCToolResultMessage = GCMessage;
/** @deprecated Use `GCMessage`. */ export type GCSystemMessage = GCMessage;
/** @deprecated Use `GCMessage`. */ export type GCStreamDelta = GCMessage;
/** @deprecated Hook input is a plain object. */ export type GCPreToolUseContext = JsonObject;
/** @deprecated Hook input is a plain object. */ export type GCHookContext = JsonObject;

// Types of the removed functions above, kept as plain objects so that code
// naming them still compiles.
/** @deprecated */ export type SandboxOptions = JsonObject;
/** @deprecated */ export type AgentManifest = JsonObject;
/** @deprecated */ export type LoadedAgent = JsonObject;
/** @deprecated */ export type McpServerConfig = JsonObject;
/** @deprecated */ export type SkillMetadata = JsonObject;
/** @deprecated */ export type WorkflowMetadata = JsonObject;
/** @deprecated */ export type SubAgentMetadata = JsonObject;
/** @deprecated */ export type ComplianceWarning = JsonObject;
/** @deprecated */ export type EnvConfig = JsonObject;
/** @deprecated */ export type SandboxConfig = JsonObject;
/** @deprecated */ export type SandboxContext = JsonObject;
/** @deprecated */ export type LocalSession = JsonObject;
/** @deprecated */ export type PluginManifest = JsonObject;
/** @deprecated */ export type PluginConfig = JsonObject;
/** @deprecated */ export type LoadedPlugin = JsonObject;
/** @deprecated */ export type GitagentPluginApi = JsonObject;
/** @deprecated */ export type ToolDefinition = JsonObject;
/** @deprecated */ export type ToolMetadata = JsonObject;
/** @deprecated */ export type SessionCosts = JsonObject;
/** @deprecated */ export type ModelUsage = JsonObject;
/** @deprecated */ export type TelemetryOptions = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type AdapterBackend = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ClientMessage = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ClientAudioMessage = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ClientVideoFrameMessage = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ClientTextMessage = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ClientFileMessage = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerMessage = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerAudioDelta = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerTranscript = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerAgentWorking = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerAgentDone = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerToolCall = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerToolResult = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerAgentThinking = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerError = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerInterrupt = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerFilesChanged = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerMemorySaving = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type ServerLogEntry = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type MultimodalAdapter = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type MultimodalAdapterConfig = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type VoiceServerOptions = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type VoiceAdapter = JsonObject;
/** @deprecated Voice mode is not part of gitagent 3. */ export type VoiceAdapterConfig = JsonObject;
