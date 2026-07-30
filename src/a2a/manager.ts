import { randomUUID } from "crypto";
import { A2AClient } from "@a2a-js/sdk/client";
import type {
	AgentCard,
	AgentSkill,
	Message,
	MessageSendParams,
	Part,
	Task,
	TaskArtifactUpdateEvent,
	TaskStatusUpdateEvent,
} from "@a2a-js/sdk";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { Type } from "@sinclair/typebox";
import type { A2AAgentConfig, A2ASetupResult } from "./types.js";

const DEFAULT_TIMEOUT_MS = 30000;

// ANSI helpers (kept local so the module has no dependency on index.ts).
const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const red = (s: string) => `\x1b[31m${s}\x1b[0m`;

/** A live connection to one remote A2A agent. */
interface A2AConnection {
	name: string;
	client: A2AClient;
	card: AgentCard;
}

function withTimeout<T>(op: Promise<T>, ms: number, label: string): Promise<T> {
	return Promise.race([
		op,
		new Promise<T>((_, reject) =>
			setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms),
		),
	]);
}

/** Replace `${VAR}` with process.env values (same syntax as plugin config). */
function interpolateEnv(value: string): string {
	return value.replace(/\$\{(\w+)\}/g, (_, name) => process.env[name] || "");
}

function interpolateHeaders(
	headers: Record<string, string> | undefined,
): Record<string, string> | undefined {
	if (!headers) return undefined;
	const out: Record<string, string> = {};
	for (const [k, v] of Object.entries(headers)) out[k] = interpolateEnv(v);
	return out;
}

/**
 * A fetch wrapper that injects static headers (auth etc.) into every request.
 * This is how the A2A SDK supports custom headers — via a custom fetch impl.
 */
function headerFetch(headers: Record<string, string>): typeof fetch {
	return (input: any, init: any = {}) => {
		const merged = new Headers(init.headers || {});
		for (const [k, v] of Object.entries(headers)) merged.set(k, v);
		return fetch(input, { ...init, headers: merged });
	};
}

/** Flatten an array of A2A content parts into plain text. Binary parts are
 *  summarized rather than inlined, to protect the token budget. */
export function partsToText(parts: Part[] | undefined): string {
	if (!Array.isArray(parts)) return "";
	const out: string[] = [];
	for (const p of parts) {
		switch (p?.kind) {
			case "text":
				out.push(p.text ?? "");
				break;
			case "file": {
				const f: any = (p as any).file ?? {};
				out.push(`[file: ${f.name || f.uri || f.mimeType || "binary"}]`);
				break;
			}
			case "data":
				try {
					out.push("```json\n" + JSON.stringify((p as any).data, null, 2) + "\n```");
				} catch {
					out.push("[data]");
				}
				break;
		}
	}
	return out.join("\n");
}

/** Extract the best text from a terminal Message or Task result. */
function resultToText(result: Message | Task): string {
	if (result.kind === "message") return partsToText(result.parts);
	// Task: prefer artifacts, then the status message.
	const fromArtifacts = (result.artifacts ?? [])
		.map((a) => partsToText(a.parts))
		.filter(Boolean)
		.join("\n");
	if (fromArtifacts) return fromArtifacts;
	const statusMsg = result.status?.message;
	if (statusMsg) return partsToText(statusMsg.parts);
	return `[task ${result.id}: ${result.status?.state ?? "unknown"}]`;
}

function buildSendParams(message: string): MessageSendParams {
	return {
		message: {
			kind: "message",
			messageId: randomUUID(),
			role: "user",
			parts: [{ kind: "text", text: message }],
		},
		configuration: {
			acceptedOutputModes: ["text/plain"],
			blocking: true,
		},
	};
}

/**
 * Run one streaming call. Accumulates text from message / status-update /
 * artifact-update events, pushing partial output via onUpdate, and returns the
 * final accumulated text.
 */
async function runStreaming(
	client: A2AClient,
	message: string,
	signal: AbortSignal | undefined,
	onUpdate?: (text: string) => void,
): Promise<string> {
	let acc = "";
	const append = (text: string) => {
		if (!text) return;
		acc += (acc ? "\n" : "") + text;
		onUpdate?.(acc);
	};
	const stream = client.sendMessageStream(buildSendParams(message));
	for await (const event of stream) {
		if (signal?.aborted) break;
		const e = event as Message | Task | TaskStatusUpdateEvent | TaskArtifactUpdateEvent;
		switch (e.kind) {
			case "message":
				append(partsToText(e.parts));
				break;
			case "status-update":
				if (e.status?.message) append(partsToText(e.status.message.parts));
				if (e.final) return acc;
				break;
			case "artifact-update":
				append(partsToText(e.artifact?.parts));
				break;
			case "task":
				// Initial/echoed task snapshot — capture any status text.
				if (e.status?.message) append(partsToText(e.status.message.parts));
				break;
		}
	}
	return acc;
}

/** Run one blocking call and return the final text. */
async function runBlocking(client: A2AClient, message: string): Promise<string> {
	const resp = await client.sendMessage(buildSendParams(message));
	if ("error" in resp && resp.error) {
		throw new Error(resp.error.message || "A2A agent returned an error");
	}
	const result = (resp as any).result as Message | Task;
	return resultToText(result);
}

/** Build the tool description from a skill (or the card, when skill-less). */
function describeSkill(card: AgentCard, skill?: AgentSkill): string {
	if (!skill) {
		return `Delegate a task to the "${card.name}" agent. ${card.description ?? ""}`.trim();
	}
	const parts = [skill.description || skill.name];
	if (skill.tags?.length) parts.push(`Tags: ${skill.tags.join(", ")}.`);
	if (skill.examples?.length) parts.push(`Examples: ${skill.examples.slice(0, 3).join(" | ")}.`);
	return `[${card.name}] ${parts.join(" ")}`.trim();
}

/** Sanitize a name so the assembled tool name is a safe identifier. */
function sanitize(name: string): string {
	return name.replace(/[^a-zA-Z0-9_]+/g, "_").replace(/^_+|_+$/g, "");
}

/**
 * Build one AgentTool for a remote agent/skill. Constructed directly (not via
 * buildTool) so streaming can push partial deltas through onUpdate.
 */
function buildA2ATool(
	conn: A2AConnection,
	config: A2AAgentConfig,
	skill: AgentSkill | undefined,
	toolName: string,
): AgentTool<any> {
	const timeoutMs = config.timeoutMs ?? DEFAULT_TIMEOUT_MS;
	const streaming = config.stream !== false && conn.card.capabilities?.streaming === true;

	return {
		name: toolName,
		label: toolName,
		description: describeSkill(conn.card, skill),
		parameters: Type.Object({
			message: Type.String({
				description:
					"The task or question to send to the remote agent, in natural language.",
			}),
		}),
		execute: async (_toolCallId, params, signal, onUpdate) => {
			const message = String((params as any)?.message ?? "").trim();
			if (!message) {
				return {
					content: [{ type: "text" as const, text: "Error: 'message' is required." }],
					details: undefined,
				};
			}
			const onText = onUpdate
				? (text: string) =>
						onUpdate({ content: [{ type: "text" as const, text }], details: undefined })
				: undefined;
			try {
				const call = streaming
					? runStreaming(conn.client, message, signal, onText)
					: runBlocking(conn.client, message);
				const text = await withTimeout(call, timeoutMs, `a2a ${conn.name}`);
				return {
					content: [{ type: "text" as const, text: text || "[no content returned]" }],
					details: undefined,
				};
			} catch (err: any) {
				const msg = err?.message ?? String(err);
				return {
					content: [
						{ type: "text" as const, text: `A2A call to "${conn.name}" failed: ${msg}` },
					],
					details: undefined,
				};
			}
		},
	};
}

/**
 * Connect to every configured A2A agent, expose each skill as a tool, and
 * return the merged tools plus an idempotent cleanup. A connection failure is
 * non-fatal: that agent is skipped with a warning and the rest still load.
 */
export async function setupA2A(
	agents: Record<string, A2AAgentConfig> | undefined,
	existingToolNames: Set<string>,
): Promise<A2ASetupResult> {
	const tools: AgentTool<any>[] = [];
	if (!agents || Object.keys(agents).length === 0) {
		return { tools, cleanup: async () => {} };
	}

	const taken = new Set(existingToolNames);

	for (const [rawName, rawConfig] of Object.entries(agents)) {
		const name = sanitize(rawName);
		if (!rawConfig?.url) {
			console.warn(red(`[a2a:${rawName}] missing "url" — skipping`));
			continue;
		}
		const config: A2AAgentConfig = {
			...rawConfig,
			url: interpolateEnv(rawConfig.url),
			headers: interpolateHeaders(rawConfig.headers),
		};
		const timeoutMs = config.timeoutMs ?? DEFAULT_TIMEOUT_MS;

		try {
			const cardUrl = config.cardPath
				? new URL(config.cardPath, config.url).toString()
				: new URL("/.well-known/agent-card.json", config.url).toString();
			const options = config.headers
				? { fetchImpl: headerFetch(config.headers) }
				: undefined;
			const client = await withTimeout(
				A2AClient.fromCardUrl(cardUrl, options),
				timeoutMs,
				`a2a ${name} connect`,
			);
			const card = await withTimeout(
				client.getAgentCard(),
				timeoutMs,
				`a2a ${name} card`,
			);
			const conn: A2AConnection = { name, client, card };

			const skills = Array.isArray(card.skills) ? card.skills : [];
			const built: AgentTool<any>[] =
				skills.length > 0
					? skills.map((skill) =>
							buildA2ATool(conn, config, skill, `${name}__${sanitize(skill.id)}`),
						)
					: [buildA2ATool(conn, config, undefined, name)];

			let added = 0;
			for (const tool of built) {
				if (taken.has(tool.name)) {
					console.warn(
						dim(`[a2a:${rawName}] tool "${tool.name}" collides — skipping`),
					);
					continue;
				}
				taken.add(tool.name);
				tools.push(tool);
				added++;
			}
			console.log(
				dim(`[a2a] ${rawName} connected (${added} skill${added === 1 ? "" : "s"})`),
			);
		} catch (err: any) {
			console.warn(
				red(`[a2a:${rawName}] connection failed: ${err?.message ?? String(err)} — skipping`),
			);
		}
	}

	// v1 holds no persistent connection between calls (each call opens its own
	// request/stream), so cleanup is a no-op placeholder kept for symmetry and
	// future streaming-connection reuse.
	const cleanup = async () => {};

	return { tools, cleanup };
}
