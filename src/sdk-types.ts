import type { AgentManifest } from "./loader.js";
import type { SessionCosts } from "./cost-tracker.js";
import type { PermissionMode, PermissionRules, CanUseTool } from "./permissions.js";

// ── Message types ──────────────────────────────────────────────────────

export type GCMessage =
	| GCAssistantMessage
	| GCUserMessage
	| GCToolUseMessage
	| GCToolResultMessage
	| GCSystemMessage
	| GCStreamDelta
	| GCPlanProposed;

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
		| "compliance_warning" | "error" | "permission_denied";
	content: string;
	metadata?: Record<string, any>;
}

/**
 * Emitted when the model calls exit_plan_mode in plan mode. The consumer
 * reviews `plan` and resolves it via query.approvePlan() / query.rejectPlan().
 */
export interface GCPlanProposed {
	type: "plan_proposed";
	plan: string;
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
	/**
	 * Permission mode. "default" gates unmatched mutating tools via canUseTool;
	 * "plan" denies all mutating tools until exit_plan_mode is approved;
	 * "acceptEdits" auto-allows write/edit; "bypassPermissions" allows all.
	 * Omitting this (and permissions/canUseTool) keeps the legacy ungated
	 * behavior for backward compatibility.
	 */
	permissionMode?: PermissionMode;
	/** allow/deny/ask rules ("Bash(git *)", "Write(src/**)"). */
	permissions?: PermissionRules;
	/** Host callback to resolve "ask" decisions. */
	canUseTool?: CanUseTool;
	/**
	 * Confine the built-in file tools (read/write/edit) to this directory —
	 * paths that resolve outside it are rejected — and run cli with it as cwd.
	 * Opt-in; unset means no jail (legacy behavior). Used by the desktop app to
	 * scope a session to its granted folder.
	 */
	rootDir?: string;
}

// ── Query interface (returned by query()) ──────────────────────────────

export interface Query extends AsyncGenerator<GCMessage, void, undefined> {
	abort(): void;
	steer(message: string): void;
	sessionId(): string;
	manifest(): AgentManifest;
	messages(): GCMessage[];
	costs(): SessionCosts;
	/**
	 * Approve a plan proposed via exit_plan_mode (resolves the pending plan and
	 * switches permission mode, default "acceptEdits"). If no plan is pending,
	 * just switches the mode.
	 */
	approvePlan(opts?: { mode?: PermissionMode }): void;
	/** Reject a pending plan with feedback; the agent stays in plan mode. */
	rejectPlan(feedback: string): void;
	/** Change the permission mode at runtime. */
	setPermissionMode(mode: PermissionMode): void;
}
