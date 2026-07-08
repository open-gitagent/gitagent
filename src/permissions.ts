import type { AgentTool } from "@mariozechner/pi-agent-core";
import { getToolMetadata } from "./tool-factory.js";

// ── Permission model (Claude Code-compatible) ──────────────────────────
//
// gitagent wraps pi-agent-core, so permissions are enforced at the
// tool-wrapper layer (mirroring wrapToolWithHooks in hooks.ts). A mutable
// PermissionState is shared by every wrapped tool and by the Query control
// methods (approvePlan / setPermissionMode), so the host can flip the mode
// at runtime — which is what makes plan-mode approval work under a wrapped
// loop.

export type PermissionMode =
	| "default"
	| "plan"
	| "acceptEdits"
	| "bypassPermissions";

export type PermissionBehavior = "allow" | "deny" | "ask";

export interface PermissionDecision {
	behavior: PermissionBehavior;
	/** Shown to the model (deny) or surfaced to the consumer. */
	message?: string;
	/** Optional rewritten args (allow path only). */
	updatedArgs?: Record<string, any>;
}

export interface PermissionRules {
	/** Auto-allow rules, e.g. "Bash(git status)", "Read", "Write(src/**)". */
	allow?: string[];
	/** Auto-deny rules — evaluated before allow. */
	deny?: string[];
	/** Force the canUseTool prompt for these calls. */
	ask?: string[];
	/** Starting mode if permissionMode is not passed to query(). */
	defaultMode?: PermissionMode;
}

export interface CanUseToolContext {
	sessionId: string;
	agentName: string;
	mode: PermissionMode;
	/** The derived match target (command, path, action…). */
	target: string;
	isReadOnly: boolean;
}

export type CanUseTool = (
	toolName: string,
	args: Record<string, any>,
	ctx: CanUseToolContext,
) => Promise<PermissionDecision> | PermissionDecision;

export interface PlanOutcome {
	approved: boolean;
	feedback?: string;
	nextMode?: PermissionMode;
}

export interface PlanDeferred {
	plan: string;
	promise: Promise<PlanOutcome>;
	resolve: (outcome: PlanOutcome) => void;
}

export interface PermissionState {
	mode: PermissionMode;
	rules: Required<Omit<PermissionRules, "defaultMode">>;
	canUseTool?: CanUseTool;
	sessionId: string;
	agentName: string;
	/** Set while exit_plan_mode is blocking, awaiting host approval. */
	planDeferred: PlanDeferred | null;
}

// ── Tool → permission descriptor ───────────────────────────────────────

export interface ToolDescriptor {
	toolName: string;
	/** String matched against rule patterns. */
	target: string;
	/** Whether the call changes the working tree / runs side effects. */
	mutates: boolean;
	args: Record<string, any>;
}

/** CC tool-name aliases → gitagent tool names, so "Bash(...)" === "cli(...)". */
const TOOL_ALIASES: Record<string, string> = {
	bash: "cli",
	shell: "cli",
	write: "write",
	edit: "edit",
	read: "read",
	grep: "grep",
	glob: "glob",
	memory: "memory",
};

function normalizeToolName(name: string): string {
	const lower = name.trim().toLowerCase();
	return TOOL_ALIASES[lower] ?? lower;
}

/**
 * How each built-in tool maps a call to a (target, mutates) pair. Tools not
 * listed here fall back to their ToolMetadata.isReadOnly flag.
 */
const TARGETS: Record<string, { target: (a: any) => string; mutates: (a: any) => boolean }> = {
	cli: { target: (a) => String(a?.command ?? ""), mutates: () => true },
	write: { target: (a) => String(a?.path ?? ""), mutates: () => true },
	edit: { target: (a) => String(a?.path ?? ""), mutates: () => true },
	read: { target: (a) => String(a?.path ?? ""), mutates: () => false },
	grep: { target: (a) => String(a?.pattern ?? a?.query ?? ""), mutates: () => false },
	glob: { target: (a) => String(a?.pattern ?? ""), mutates: () => false },
	// memory mutates only on save; load is read-only.
	memory: { target: (a) => String(a?.action ?? ""), mutates: (a) => a?.action === "save" },
	capture_photo: { target: () => "", mutates: () => false },
	// Internal bookkeeping under .gitagent/learning — safe during plan mode.
	task_tracker: { target: (a) => String(a?.action ?? ""), mutates: () => false },
	// Only the actions that write/commit a SKILL.md count as mutating.
	skill_learner: {
		target: (a) => String(a?.action ?? ""),
		mutates: (a) => ["crystallize", "update", "delete"].includes(a?.action),
	},
};

function firstStringArg(args: Record<string, any>): string {
	for (const v of Object.values(args ?? {})) {
		if (typeof v === "string") return v;
	}
	return "";
}

export function describeToolCall(tool: AgentTool<any>, args: any): ToolDescriptor {
	const name = tool.name;
	const spec = TARGETS[name];
	if (spec) {
		return { toolName: name, target: spec.target(args), mutates: spec.mutates(args), args: args ?? {} };
	}
	const meta = getToolMetadata(tool);
	return {
		toolName: name,
		target: firstStringArg(args),
		mutates: !meta.isReadOnly,
		args: args ?? {},
	};
}

// ── Rule parsing + matching ────────────────────────────────────────────

interface ParsedRule {
	tool: string;
	pattern?: string;
}

function parseRule(rule: string): ParsedRule | null {
	const m = rule.match(/^([A-Za-z_][\w-]*)\s*(?:\(([\s\S]*)\))?$/);
	if (!m) return null;
	return { tool: normalizeToolName(m[1]), pattern: m[2] !== undefined ? m[2].trim() : undefined };
}

function escapeRegex(s: string): string {
	return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Glob match supporting `*` and CC's `:*` prefix convention. */
function globMatch(pattern: string, target: string): boolean {
	const normalized = pattern.replace(/:\*/g, "*");
	const re = "^" + normalized.split("*").map(escapeRegex).join(".*") + "$";
	return new RegExp(re, "s").test(target);
}

function ruleMatches(rule: string, descriptor: ToolDescriptor): boolean {
	const parsed = parseRule(rule);
	if (!parsed) return false;
	if (parsed.tool !== descriptor.toolName) return false;
	if (parsed.pattern === undefined || parsed.pattern === "") return true;
	return globMatch(parsed.pattern, descriptor.target);
}

function matchesAny(rules: string[], descriptor: ToolDescriptor): boolean {
	return rules.some((r) => ruleMatches(r, descriptor));
}

// ── State construction ─────────────────────────────────────────────────

export function normalizeRules(rules?: PermissionRules): Required<Omit<PermissionRules, "defaultMode">> {
	return {
		allow: rules?.allow ?? [],
		deny: rules?.deny ?? [],
		ask: rules?.ask ?? [],
	};
}

/** Appended to the system prompt while in plan mode. */
export const PLAN_MODE_PROMPT = `
[PLAN MODE ACTIVE]
You are in plan mode. You MUST NOT make any changes yet: no writing/editing files,
no side-effecting shell commands, no committing. Only read-only research is allowed
(reading files, searching, inspecting). Mutating tools will be denied.

When you have gathered enough understanding, call the exit_plan_mode tool with a
concise, concrete implementation plan (markdown). Do not start implementing until the
user approves the plan — after approval you will be told to proceed.`.trim();

export function planDenyMessage(toolName: string): string {
	return (
		`Cannot use "${toolName}" while in plan mode. Do read-only research only, ` +
		`then call exit_plan_mode with your implementation plan so the user can ` +
		`approve it before any changes are made.`
	);
}

// ── Core evaluation ────────────────────────────────────────────────────

/**
 * Resolve a tool call to allow/deny. The "ask" behavior is resolved here via
 * canUseTool, so callers only ever see allow or deny.
 */
export async function evaluatePermission(
	descriptor: ToolDescriptor,
	state: PermissionState,
): Promise<PermissionDecision> {
	const { mode, rules } = state;

	if (mode === "bypassPermissions") return { behavior: "allow" };

	// Deny rules win over everything except bypass.
	if (matchesAny(rules.deny, descriptor)) {
		return { behavior: "deny", message: `Tool "${descriptor.toolName}" denied by a deny rule.` };
	}
	if (matchesAny(rules.allow, descriptor)) return { behavior: "allow" };

	if (mode === "plan") {
		if (!descriptor.mutates) return { behavior: "allow" };
		return { behavior: "deny", message: planDenyMessage(descriptor.toolName) };
	}

	if (mode === "acceptEdits" && (descriptor.toolName === "write" || descriptor.toolName === "edit")) {
		return { behavior: "allow" };
	}

	// Read-only calls are allowed unless an explicit ask rule targets them.
	if (!descriptor.mutates && !matchesAny(rules.ask, descriptor)) {
		return { behavior: "allow" };
	}

	// Ask rule, or an unmatched mutating call in default mode → prompt the host.
	return resolveAsk(descriptor, state);
}

async function resolveAsk(
	descriptor: ToolDescriptor,
	state: PermissionState,
): Promise<PermissionDecision> {
	if (!state.canUseTool) {
		return {
			behavior: "deny",
			message:
				`Tool "${descriptor.toolName}" needs approval but no canUseTool callback ` +
				`was provided. Pass canUseTool, add an allow rule, or use ` +
				`permissionMode: "bypassPermissions".`,
		};
	}
	const ctx: CanUseToolContext = {
		sessionId: state.sessionId,
		agentName: state.agentName,
		mode: state.mode,
		target: descriptor.target,
		isReadOnly: !descriptor.mutates,
	};
	const decision = await state.canUseTool(descriptor.toolName, descriptor.args, ctx);
	if (decision.behavior === "ask") {
		return { behavior: "deny", message: decision.message ?? "Tool call left unresolved by canUseTool." };
	}
	return decision;
}

// ── Tool wrapper ───────────────────────────────────────────────────────

/**
 * Wrap a tool's execute with the permission gate. Mirrors wrapToolWithHooks.
 * exit_plan_mode is never gated. A denial throws (so the model sees a tool
 * error and can adapt) and fires onDenied so the consumer gets a system
 * message.
 */
export function wrapToolWithPermissions<T extends AgentTool<any>>(
	tool: T,
	state: PermissionState,
	onDenied: (toolName: string, message: string) => void,
): T {
	if (tool.name === "exit_plan_mode") return tool;

	const originalExecute = tool.execute;
	const wrapped = {
		...tool,
		execute: async (toolCallId: string, args: any, signal?: AbortSignal, onUpdate?: any) => {
			const descriptor = describeToolCall(tool, args);
			const decision = await evaluatePermission(descriptor, state);
			if (decision.behavior === "deny") {
				const message = decision.message || `Tool "${tool.name}" denied by permission policy.`;
				onDenied(tool.name, message);
				throw new Error(message);
			}
			const finalArgs = decision.updatedArgs ?? args;
			return originalExecute.call(tool, toolCallId, finalArgs, signal, onUpdate);
		},
	};
	return wrapped as T;
}
