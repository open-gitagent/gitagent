import type { AgentTool } from "@mariozechner/pi-agent-core";

/**
 * A remote A2A (Agent2Agent) agent gitagent is allowed to call. Outbound only —
 * gitagent connects out to the agent's Agent Card; it never runs a server.
 */
export interface A2AAgentConfig {
	/** Base URL of the remote agent. The Agent Card is resolved from here
	 *  (`/.well-known/agent-card.json` unless `cardPath` overrides it). */
	url: string;
	/** Explicit Agent Card path/URL, if the agent doesn't use the well-known path. */
	cardPath?: string;
	/** Extra HTTP headers (e.g. auth) sent on every request. `${VAR}` is interpolated. */
	headers?: Record<string, string>;
	/** Connect + per-call timeout in ms. Default 30000. */
	timeoutMs?: number;
	/** Use SSE streaming (`message/stream`) when the agent supports it. Default true.
	 *  Set false to force blocking `message/send`. */
	stream?: boolean;
}

/** Result of wiring up all configured A2A agents for a session. */
export interface A2ASetupResult {
	/** Tools discovered across all agents, one per skill, namespaced `<agent>__<skill>`. */
	tools: AgentTool<any>[];
	/** Idempotent teardown — aborts any open streams. Safe to call repeatedly. */
	cleanup: () => Promise<void>;
}
