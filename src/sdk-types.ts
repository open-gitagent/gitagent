import type { AgentManifest } from "./loader.js";
import type { SessionCosts } from "./cost-tracker.js";
import type { A2AAgentConfig } from "./a2a/types.js";

// ── Message types ──────────────────────────────────────────────────────

export type GCMessage =
	| GCAssistantMessage
	| GCUserMessage
	| GCToolUseMessage
	| GCToolResultMessage
	| GCSystemMessage
	| GCStreamDelta;

export interface GCAssistantMessage {
	type: "assistant";
	content: string;
	thinking?: string;
	model: string;
	provider: string;
	stopReason: "stop" | "length" | "toolUse" | "error" | "aborted";
	errorMessage?: string;
	usage?: {
		inputTokens: number;
		outputTokens: number;
		cacheReadTokens: number;
		cacheWriteTokens: number;
		totalTokens: number;
		costUsd: number;
	};
}

export interface GCUserMessage {
	type: "user";
	content: string;
}

export interface GCToolUseMessage {
	type: "tool_use";
	toolCallId: string;
	toolName: string;
	args: Record<string, any>;
}

export interface GCToolResultMessage {
	type: "tool_result";
	toolCallId: string;
	toolName: string;
	content: string;
	isError: boolean;
}

export interface GCSystemMessage {
	type: "system";
	subtype: "session_start" | "session_end" | "hook_blocked"
		| "compliance_warning" | "error";
	content: string;
	metadata?: Record<string, any>;
}

export interface GCStreamDelta {
	type: "delta";
	deltaType: "text" | "thinking";
	content: string;
}

// ── Hook types ─────────────────────────────────────────────────────────

export type GCHookEvent = "SessionStart" | "PreToolUse" | "PostToolFailure" | "PreQuery" | "PostResponse" | "FileChanged" | "OnError";

export interface GCHookContext {
	sessionId: string;
	agentName: string;
	event: GCHookEvent;
}

export interface GCPreToolUseContext extends GCHookContext {
	event: "PreToolUse";
	toolName: string;
	args: Record<string, any>;
}

export interface GCHookResult {
	action: "allow" | "block" | "modify";
	reason?: string;
	args?: Record<string, any>;
}

export interface GCHooks {
	onSessionStart?: (ctx: GCHookContext) => Promise<GCHookResult> | GCHookResult;
	preToolUse?: (ctx: GCPreToolUseContext) => Promise<GCHookResult> | GCHookResult;
	postToolFailure?: (ctx: GCHookContext & { toolName: string; error: string }) => Promise<void> | void;
	preQuery?: (ctx: GCHookContext) => Promise<GCHookResult> | GCHookResult;
	postResponse?: (ctx: GCHookContext) => Promise<void> | void;
	fileChanged?: (ctx: GCHookContext & { path: string }) => Promise<void> | void;
	onError?: (ctx: GCHookContext & { error: string }) => Promise<void> | void;
}

// ── Tool definition ────────────────────────────────────────────────────

export interface GCToolDefinition {
	name: string;
	description: string;
	inputSchema: Record<string, any>;
	handler: (args: any, signal?: AbortSignal) => Promise<string | { text: string; details?: any }>;
}

// ── Local repo options ──────────────────────────────────────────────────

export interface LocalRepoOptions {
	url: string;
	token: string;
	dir?: string;
	session?: string;
}

// ── Sandbox options ─────────────────────────────────────────────────────

export interface SandboxOptions {
	provider: "e2b";
	template?: string;
	timeout?: number;
	repository?: string;
	token?: string;
	session?: string;
	autoCommit?: boolean;
	envs?: Record<string, string>;
}

// ── Query options ──────────────────────────────────────────────────────

export interface QueryOptions {
	prompt: string | AsyncIterable<GCUserMessage>;
	dir?: string;
	model?: string;
	env?: string;
	systemPrompt?: string;
	systemPromptSuffix?: string;
	tools?: GCToolDefinition[];
	/**
	 * Remote A2A agents to connect to, defined in code. Merged on top of the
	 * agent.yaml `a2a_agents` map (these win on key collision). Each remote skill
	 * becomes a `<key>__<skill>` tool, same as the manifest-declared ones.
	 */
	a2aAgents?: Record<string, A2AAgentConfig>;
	replaceBuiltinTools?: boolean;
	allowedTools?: string[];
	disallowedTools?: string[];
	repo?: LocalRepoOptions;
	sandbox?: SandboxOptions | boolean;
	hooks?: GCHooks;
	maxTurns?: number;
	abortController?: AbortController;
	sessionId?: string;
	constraints?: {
		temperature?: number;
		maxTokens?: number;
		topP?: number;
		topK?: number;
	};
}

// ── Query interface (returned by query()) ──────────────────────────────

export interface Query extends AsyncGenerator<GCMessage, void, undefined> {
	abort(): void;
	steer(message: string): void;
	sessionId(): string;
	manifest(): AgentManifest;
	messages(): GCMessage[];
	costs(): SessionCosts;
}
