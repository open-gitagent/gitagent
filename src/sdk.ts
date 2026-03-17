import { Agent } from "@mariozechner/pi-agent-core";
import type { AgentEvent, AgentTool } from "@mariozechner/pi-agent-core";
import type { AssistantMessage } from "@mariozechner/pi-ai";
import { loadAgent } from "./loader.js";
import type { AgentManifest } from "./loader.js";
import { createBuiltinTools } from "./tools/index.js";
import { createSandboxContext } from "./sandbox.js";
import type { SandboxContext } from "./sandbox.js";
import { loadHooksConfig, runHooks, wrapToolWithHooks } from "./hooks.js";
import { loadDeclarativeTools } from "./tool-loader.js";
import { toAgentTool } from "./tool-utils.js";
import { wrapToolWithProgrammaticHooks } from "./sdk-hooks.js";
import { mergeHooksConfigs } from "./plugins.js";
import { initLocalSession } from "./session.js";
import type { LocalSession } from "./session.js";
import type {
	GCMessage,
	GCAssistantMessage,
	GCToolDefinition,
	GCHookContext,
	Query,
	QueryOptions,
	SandboxOptions,
} from "./sdk-types.js";

// ── Event channel ──────────────────────────────────────────────────────

interface Channel<T> {
	push(v: T): void;
	finish(): void;
	pull(): Promise<IteratorResult<T>>;
}

function createChannel<T>(): Channel<T> {
	const buffer: T[] = [];
	let resolve: ((v: IteratorResult<T>) => void) | null = null;
	let done = false;

	return {
		push(v: T) {
			if (resolve) {
				resolve({ value: v, done: false });
				resolve = null;
			} else {
				buffer.push(v);
			}
		},
		finish() {
			done = true;
			if (resolve) {
				resolve({ value: undefined as any, done: true });
				resolve = null;
			}
		},
		pull(): Promise<IteratorResult<T>> {
			if (buffer.length) {
				return Promise.resolve({ value: buffer.shift()!, done: false });
			}
			if (done) {
				return Promise.resolve({ value: undefined as any, done: true });
			}
			return new Promise((r) => { resolve = r; });
		},
	};
}

// ── Extract text/thinking from AssistantMessage ────────────────────────

function extractContent(msg: AssistantMessage): { text: string; thinking: string } {
	let text = "";
	let thinking = "";
	for (const block of msg.content) {
		if (block.type === "text") text += block.text;
		if (block.type === "thinking") thinking += block.thinking;
	}
	return { text, thinking };
}

// ── query() ────────────────────────────────────────────────────────────

export function query(options: QueryOptions): Query {
	const channel = createChannel<GCMessage>();
	const collectedMessages: GCMessage[] = [];
	const ac = options.abortController ?? new AbortController();

	// These are set once the agent is loaded (async init below)
	let _sessionId = options.sessionId ?? "";
	let _manifest: AgentManifest | null = null;

	// Accumulate streaming deltas for the current message
	let accText = "";
	let accThinking = "";

	function pushMsg(msg: GCMessage) {
		collectedMessages.push(msg);
		channel.push(msg);
	}

	// Sandbox context (hoisted for cleanup in catch)
	let sandboxCtx: SandboxContext | undefined;
	// Local session (hoisted for cleanup in catch)
	let localSession: LocalSession | undefined;

	// Async initialization + run
	const runPromise = (async () => {
		// Validate mutually exclusive options
		if (options.repo && options.sandbox) {
			throw new Error("repo and sandbox options are mutually exclusive");
		}

		let dir = options.dir ?? process.cwd();

		// Local repo mode
		if (options.repo) {
			const token = options.repo.token || process.env.GITHUB_TOKEN || process.env.GIT_TOKEN;
			if (!token) {
				throw new Error("repo.token, GITHUB_TOKEN, or GIT_TOKEN is required with repo option");
			}
			localSession = initLocalSession({
				url: options.repo.url,
				token,
				dir: options.repo.dir || dir,
				session: options.repo.session,
			});
			dir = localSession.dir;
		}

		// 1. Load agent
		const loaded = await loadAgent(dir, options.model, options.env);
		_manifest = loaded.manifest;
		_sessionId = _sessionId || loaded.sessionId;

		// 2. Apply system prompt overrides
		let systemPrompt = loaded.systemPrompt;
		if (options.systemPrompt !== undefined) {
			systemPrompt = options.systemPrompt;
		}
		if (options.systemPromptSuffix) {
			systemPrompt += "\n\n" + options.systemPromptSuffix;
		}

		// 3. Build tools (with optional sandbox)
		if (options.sandbox) {
			const sandboxConfig: SandboxOptions = options.sandbox === true
				? { provider: "e2b" }
				: options.sandbox;
			sandboxCtx = await createSandboxContext(sandboxConfig, dir);
			await sandboxCtx.gitMachine.start();
		}

		// Collect plugin memory layers
		const pluginMemoryLayers = loaded.plugins.flatMap((p) => p.memoryLayers);

		let tools: AgentTool<any>[] = [];

		if (!options.replaceBuiltinTools) {
			tools = createBuiltinTools({
				dir,
				timeout: loaded.manifest.runtime.timeout,
				sandbox: sandboxCtx,
				gitagentDir: loaded.gitagentDir,
				pluginMemoryLayers: pluginMemoryLayers.length > 0 ? pluginMemoryLayers : undefined,
			});
		}

		// Declarative tools from tools/*.yaml
		const declarativeTools = await loadDeclarativeTools(loaded.agentDir);
		tools = [...tools, ...declarativeTools];

		// Plugin tools (declarative + programmatic) — check for collisions with existing tools
		const existingToolNames = new Set(tools.map((t) => t.name));
		for (const plugin of loaded.plugins) {
			const pluginTools = [
				...plugin.tools,
				...plugin.programmaticTools.map(toAgentTool),
			];
			for (const t of pluginTools) {
				if (existingToolNames.has(t.name)) {
					console.warn(`[plugin:${plugin.manifest.id}] Tool "${t.name}" collides with existing tool — skipping`);
				} else {
					tools.push(t);
					existingToolNames.add(t.name);
				}
			}
		}

		// SDK-provided tools
		if (options.tools) {
			const converted = options.tools.map(toAgentTool);
			tools = [...tools, ...converted];
			console.error(`[sdk] Injected ${converted.length} external tools: ${converted.map(t => t.name).join(", ")}`);
		}
		console.error(`[sdk] Total tools before filtering: ${tools.length} → ${tools.map(t => t.name).join(", ")}`);

		// Filter by allowlist/denylist
		if (options.allowedTools) {
			const allowed = new Set(options.allowedTools);
			tools = tools.filter((t) => allowed.has(t.name));
		}
		if (options.disallowedTools) {
			const denied = new Set(options.disallowedTools);
			tools = tools.filter((t) => !denied.has(t.name));
		}

		// 4. Wrap with script-based hooks (agent + plugin hooks merged)
		const agentHooksConfig = await loadHooksConfig(loaded.agentDir);
		const hooksConfig = mergeHooksConfigs(agentHooksConfig, loaded.plugins);
		if (hooksConfig) {
			tools = tools.map((t) =>
				wrapToolWithHooks(t, hooksConfig, loaded.agentDir, _sessionId),
			);
		}

		// 5. Wrap with programmatic hooks
		if (options.hooks) {
			tools = tools.map((t) =>
				wrapToolWithProgrammaticHooks(t, options.hooks!, _sessionId, loaded.manifest.name),
			);
		}

		// 6. Run on_session_start hooks (script-based)
		if (hooksConfig?.hooks.on_session_start) {
			const result = await runHooks(hooksConfig.hooks.on_session_start, loaded.agentDir, {
				event: "on_session_start",
				session_id: _sessionId,
				agent: loaded.manifest.name,
			});
			if (result.action === "block") {
				pushMsg({
					type: "system",
					subtype: "hook_blocked",
					content: `Session blocked by hook: ${result.reason || "no reason given"}`,
				});
				channel.finish();
				return;
			}
		}

		// 6b. Run on_session_start programmatic hook
		if (options.hooks?.onSessionStart) {
			const ctx: GCHookContext = {
				sessionId: _sessionId,
				agentName: loaded.manifest.name,
				event: "SessionStart",
			};
			const result = await options.hooks.onSessionStart(ctx);
			if (result.action === "block") {
				pushMsg({
					type: "system",
					subtype: "hook_blocked",
					content: `Session blocked by hook: ${result.reason || "no reason given"}`,
				});
				channel.finish();
				return;
			}
		}

		// 7. Build model options from constraints
		const modelOptions: Record<string, any> = {};
		const constraints = options.constraints ?? loaded.manifest.model.constraints;
		if (constraints) {
			const c = constraints as any;
			if (c.temperature !== undefined) modelOptions.temperature = c.temperature;
			if (c.maxTokens !== undefined) modelOptions.maxTokens = c.maxTokens;
			if (c.max_tokens !== undefined) modelOptions.maxTokens = c.max_tokens;
			if (c.topP !== undefined) modelOptions.topP = c.topP;
			if (c.top_p !== undefined) modelOptions.topP = c.top_p;
			if (c.topK !== undefined) modelOptions.topK = c.topK;
			if (c.top_k !== undefined) modelOptions.topK = c.top_k;
		}

		if (options.maxTurns !== undefined) {
			modelOptions.maxTurns = options.maxTurns;
		}

		// 8. Create Agent
		const agent = new Agent({
			initialState: {
				systemPrompt,
				model: loaded.model,
				tools,
				...modelOptions,
			},
		});

		// 9. Subscribe to events and map to GCMessage
		agent.subscribe((event: AgentEvent) => {
			switch (event.type) {
				case "agent_start":
					pushMsg({
						type: "system",
						subtype: "session_start",
						content: `Agent ${loaded.manifest.name} started`,
						metadata: { sessionId: _sessionId },
					});
					break;

				case "message_update": {
					const e = event.assistantMessageEvent;
					if (e.type === "text_delta") {
						accText += e.delta;
						pushMsg({
							type: "delta",
							deltaType: "text",
							content: e.delta,
						});
					} else if (e.type === "thinking_delta") {
						accThinking += e.delta;
						pushMsg({
							type: "delta",
							deltaType: "thinking",
							content: e.delta,
						});
					}
					break;
				}

				case "message_end": {
					// Only process assistant messages — skip user/toolResult
					const raw = event.message as any;
					if (!raw || raw.role !== "assistant") break;

					const msg = raw as AssistantMessage;

					// Emit error system message if the LLM call failed
					if (msg.stopReason === "error") {
						pushMsg({
							type: "system",
							subtype: "error",
							content: msg.errorMessage || "LLM request failed (unknown error)",
							metadata: {
								model: msg.model,
								provider: msg.provider,
								api: (msg as any).api,
							},
						});
						// Still emit the assistant message so callers can inspect stopReason
					}

					const { text, thinking } = extractContent(msg);

					const assistantMsg: GCAssistantMessage = {
						type: "assistant",
						content: text || accText,
						thinking: (thinking || accThinking) || undefined,
						model: msg.model ?? "unknown",
						provider: msg.provider ?? "unknown",
						stopReason: msg.stopReason ?? "stop",
						errorMessage: msg.errorMessage,
						usage: msg.usage ? {
							inputTokens: msg.usage.input ?? 0,
							outputTokens: msg.usage.output ?? 0,
							cacheReadTokens: msg.usage.cacheRead ?? 0,
							cacheWriteTokens: msg.usage.cacheWrite ?? 0,
							totalTokens: msg.usage.totalTokens ?? 0,
							costUsd: msg.usage.cost?.total ?? 0,
						} : undefined,
					};
					pushMsg(assistantMsg);

					// Reset accumulators
					accText = "";
					accThinking = "";

					// Fire post_response hooks (non-blocking)
					if (hooksConfig?.hooks.post_response) {
						runHooks(hooksConfig.hooks.post_response, loaded.agentDir, {
							event: "post_response",
							session_id: _sessionId,
						}).catch(() => {});
					}
					if (options.hooks?.postResponse) {
						Promise.resolve(options.hooks.postResponse({
							sessionId: _sessionId,
							agentName: loaded.manifest.name,
							event: "PostResponse",
						})).catch(() => {});
					}
					break;
				}

				case "tool_execution_start":
					pushMsg({
						type: "tool_use",
						toolCallId: event.toolCallId,
						toolName: event.toolName,
						args: event.args ?? {},
					});
					break;

				case "tool_execution_end": {
					const text = event.result?.content?.[0]?.text ?? "";
					pushMsg({
						type: "tool_result",
						toolCallId: event.toolCallId,
						toolName: event.toolName,
						content: text,
						isError: event.isError,
					});
					break;
				}

				case "agent_end":
					pushMsg({
						type: "system",
						subtype: "session_end",
						content: `Agent ${loaded.manifest.name} finished`,
						metadata: { sessionId: _sessionId },
					});
					channel.finish();
					break;
			}
		});

		// 10. Send prompt
		if (typeof options.prompt === "string") {
			await agent.prompt(options.prompt);
		} else {
			// Multi-turn: iterate the async iterable
			for await (const userMsg of options.prompt) {
				pushMsg({ type: "user", content: userMsg.content });
				await agent.prompt(userMsg.content);
			}
		}

		// Finalize local session if active
		if (localSession) {
			try { localSession.finalize(); } catch { /* best-effort */ }
		}

		// Stop sandbox if active
		if (sandboxCtx) {
			await sandboxCtx.gitMachine.stop().catch(() => {});
		}

		// Ensure channel finishes even if no agent_end event
		channel.finish();
	})().catch(async (err) => {
		// Finalize local session on error
		if (localSession) {
			try { localSession.finalize(); } catch { /* best-effort */ }
		}

		// Stop sandbox on error
		if (sandboxCtx) {
			await sandboxCtx.gitMachine.stop().catch(() => {});
		}

		// Fire on_error hooks
		if (options.hooks?.onError) {
			Promise.resolve(options.hooks.onError({
				sessionId: _sessionId,
				agentName: _manifest?.name ?? "unknown",
				event: "OnError",
				error: err.message,
			})).catch(() => {});
		}
		pushMsg({
			type: "system",
			subtype: "error",
			content: err.message,
		});
		channel.finish();
	});

	// Build the Query object (AsyncGenerator + helpers)
	const generator: Query = {
		abort() {
			ac.abort();
		},

		steer(_message: string) {
			// Steering requires agent reference — for now this is a placeholder.
			// Full steering support would require exposing the Agent instance.
		},

		sessionId() {
			return _sessionId;
		},

		manifest() {
			if (!_manifest) throw new Error("Agent not yet loaded");
			return _manifest;
		},

		messages() {
			return [...collectedMessages];
		},

		// AsyncGenerator protocol
		next() {
			return channel.pull();
		},

		return(value?: any) {
			channel.finish();
			return Promise.resolve({ value, done: true as const });
		},

		throw(err?: any) {
			channel.finish();
			return Promise.reject(err);
		},

		[Symbol.asyncIterator]() {
			return generator;
		},
	};

	return generator;
}

// ── tool() helper ──────────────────────────────────────────────────────

export function tool(
	name: string,
	description: string,
	inputSchema: Record<string, any>,
	handler: (args: any, signal?: AbortSignal) => Promise<string | { text: string; details?: any }>,
): GCToolDefinition {
	return { name, description, inputSchema, handler };
}
