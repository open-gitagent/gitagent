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
import { setupMcp } from "./mcp/manager.js";
import type { McpSetupResult } from "./mcp/types.js";
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
import { CostTracker } from "./cost-tracker.js";
import { isRetryableProviderError } from "./model-fallback.js";
import { context as otelContext } from "@opentelemetry/api";
import {
	wrapToolWithOtel,
	startSessionSpan,
	recordGenAiCall,
} from "./telemetry.js";

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
	const costTracker = new CostTracker();

	// These are set once the agent is loaded (async init below)
	let _sessionId = options.sessionId ?? "";
	let _manifest: AgentManifest | null = null;

	// Accumulate streaming deltas for the current message
	let accText = "";
	let accThinking = "";

	// Track tool args by toolCallId so file_changed hook can access them
	const toolArgsMap = new Map<string, any>();

	function pushMsg(msg: GCMessage) {
		collectedMessages.push(msg);
		channel.push(msg);
	}

	// Sandbox context (hoisted for cleanup in catch)
	let sandboxCtx: SandboxContext | undefined;
	let mcpSetup: McpSetupResult | undefined;
	// Local session (hoisted for cleanup in catch)
	let localSession: LocalSession | undefined;

	// OpenTelemetry session span — opened immediately so it covers agent
	// load + prompt + cleanup. Closed in the IIFE's finally so every exit
	// path (success, hook-block early-return, thrown error) ends it exactly
	// once.
	const _session = startSessionSpan("gitagent.agent.session", {
		"gitagent.entry": "sdk",
	});
	let _llmCallStart = 0;
	let _totalCostUsd = 0;

	// Async initialization + run
	const runPromise = (async () => {
		try {
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

		// MCP tools — merge manifest + SDK server configs (SDK wins on key collision)
		const mcpServers = { ...loaded.manifest.mcp_servers, ...options.mcpServers };
		mcpSetup = await setupMcp(mcpServers, existingToolNames);
		tools = [...tools, ...mcpSetup.tools];

		// SDK-provided tools
		if (options.tools) {
			const converted = options.tools.map(toAgentTool);
			tools = [...tools, ...converted];
		}

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

		// 5b. Wrap every tool with OpenTelemetry instrumentation. No-op if
		// telemetry isn't initialised — wrapToolWithOtel returns the tool
		// unchanged in that case.
		tools = tools.map(wrapToolWithOtel);

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

		// 8. Fallback-aware agent runner.
		// The manifest's preferred model plus any resolved fallbacks form an
		// ordered candidate list. When a model fails with a retryable provider
		// error before producing output (e.g. "credit balance too low"), the
		// runner swaps the model on the same agent and retries (issue #24).
		const candidateModels = [loaded.model, ...loaded.fallbackModels];

		// Controller state shared across attempts.
		let sessionStartEmitted = false;
		interface AttemptState {
			producedOutput: boolean;
			error: { message?: string; provider?: string; model?: string; api?: string } | null;
		}
		let attempt: AttemptState = { producedOutput: false, error: null };

		// 9. Build an Agent for a given model and wire event → GCMessage mapping.
		const buildAgent = (model: typeof loaded.model): Agent => {
		const agent = new Agent({
			initialState: {
				systemPrompt,
				model,
				tools,
				...modelOptions,
			},
		});

		agent.subscribe((event: AgentEvent) => {
			switch (event.type) {
				case "agent_start":
					// Emit once even across fallback attempts.
					if (!sessionStartEmitted) {
						sessionStartEmitted = true;
						pushMsg({
							type: "system",
							subtype: "session_start",
							content: `Agent ${loaded.manifest.name} started`,
							metadata: { sessionId: _sessionId },
						});
					}
					break;

				case "message_update": {
					const e = event.assistantMessageEvent;
					// Capture the start of this LLM turn on the first delta so
					// recordGenAiCall has a duration. (pi-agent-core does not
					// expose a message_start event in its public union.)
					if (_llmCallStart === 0) {
						_llmCallStart = Date.now();
					}
					if (e.type === "text_delta") {
						attempt.producedOutput = true;
						accText += e.delta;
						pushMsg({
							type: "delta",
							deltaType: "text",
							content: e.delta,
						});
					} else if (e.type === "thinking_delta") {
						attempt.producedOutput = true;
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

					// Buffer a failed LLM call instead of emitting immediately — the
					// fallback runner decides whether to retry on the next model or
					// surface it. Only emitted (below, or by the runner) when terminal.
					if (msg.stopReason === "error") {
						attempt.error = {
							message: msg.errorMessage || "LLM request failed (unknown error)",
							model: msg.model,
							provider: msg.provider,
							api: (msg as any).api,
						};
						// Reset accumulators + LLM-call timer (a partial stream may have
						// set it) and skip cost tracking (usage is empty on error).
						accText = "";
						accThinking = "";
						_llmCallStart = 0;
						break;
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

					// Track costs per model
					if (assistantMsg.usage) {
						costTracker.add(
							`${assistantMsg.provider}:${assistantMsg.model}`,
							assistantMsg.usage,
						);
						_totalCostUsd += assistantMsg.usage.costUsd ?? 0;
					}

					// Emit gen_ai.chat span (no-op if telemetry disabled).
					try {
						const durationMs =
							_llmCallStart > 0 ? Date.now() - _llmCallStart : 0;
						recordGenAiCall(msg, { durationMs });
					} catch {
						/* never let telemetry break the agent */
					}
					_llmCallStart = 0;

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
					// A tool ran, so the model committed to output — no safe retry.
					attempt.producedOutput = true;
					toolArgsMap.set(event.toolCallId, event.args ?? {});
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

					// Fire post_tool_failure hooks
					if (event.isError && hooksConfig?.hooks.post_tool_failure) {
						runHooks(hooksConfig.hooks.post_tool_failure, loaded.agentDir, {
							event: "post_tool_failure",
							session_id: _sessionId,
							tool: event.toolName,
							error: text,
						}).catch(() => {});
					}

					// Fire file_changed hooks for write/edit tools
					if (!event.isError && hooksConfig?.hooks.file_changed &&
						(event.toolName === "write" || event.toolName === "edit")) {
						const toolArgs = toolArgsMap.get(event.toolCallId) ?? {};
						runHooks(hooksConfig.hooks.file_changed, loaded.agentDir, {
							event: "file_changed",
							session_id: _sessionId,
							tool: event.toolName,
							file_path: toolArgs.path ?? "",
						}).catch(() => {});
					}
					toolArgsMap.delete(event.toolCallId);
					break;
				}

				case "agent_end": {
					// A thrown provider error (network reset, immediate reject) is
					// reported by pi-agent-core via handleRunFailure, which emits
					// agent_end with a trailing errored assistant message and NO
					// message_end. Capture that here so fallback still triggers.
					if (!attempt.error) {
						const last = event.messages?.[event.messages.length - 1] as any;
						if (last?.role === "assistant" && last.stopReason === "error") {
							attempt.error = {
								message: last.errorMessage || "LLM request failed (unknown error)",
								model: last.model,
								provider: last.provider,
								api: last.api,
							};
						}
					}
					// Do not finish the channel here — the fallback runner may start
					// another attempt. session_end + finish happen once, after the
					// runner settles (see finalizeRun()).
					break;
				}
			}
		});

		return agent;
		};

		// Emit session_end once and close the stream. Called after all prompts
		// (and any fallback retries) have settled.
		let runFinalized = false;
		const finalizeRun = () => {
			if (runFinalized) return;
			runFinalized = true;
			pushMsg({
				type: "system",
				subtype: "session_end",
				content: `Agent ${loaded.manifest.name} finished`,
				metadata: { sessionId: _sessionId },
			});
			channel.finish();
		};

		// One persistent agent for the whole run so multi-turn conversation state
		// survives across prompts. Fallback swaps the model on this same agent
		// (and restores the pre-attempt transcript) rather than rebuilding it.
		const agent = buildAgent(candidateModels[0]);
		let activeModelIndex = 0;

		// Run one prompt, falling back through candidate models when the current
		// model fails with a retryable provider error before producing any output.
		const runPrompt = async (promptText: string): Promise<void> => {
			for (let i = activeModelIndex; i < candidateModels.length; i++) {
				attempt = { producedOutput: false, error: null };
				if (i !== activeModelIndex) {
					agent.state.model = candidateModels[i];
					activeModelIndex = i;
				}
				// Snapshot transcript so a failed attempt can be rolled back cleanly.
				const snapshot = [...agent.state.messages];
				await otelContext.with(_session.ctx, () => agent.prompt(promptText));

				const err = attempt.error;
				if (!err) return; // success — stay on this model for later turns

				const hasNext = i < candidateModels.length - 1;
				const canRetry =
					hasNext && !attempt.producedOutput && isRetryableProviderError(err.message);

				if (canRetry) {
					const next = candidateModels[i + 1];
					// Drop the failed user+assistant turn so the retry starts clean.
					agent.state.messages = snapshot;
					pushMsg({
						type: "system",
						subtype: "fallback",
						content:
							`Model ${err.provider ?? ""}:${err.model ?? ""} failed (${err.message}). ` +
							`Falling back to ${next.provider}:${next.id}.`,
						metadata: {
							failedModel: `${err.provider ?? ""}:${err.model ?? ""}`,
							nextModel: `${next.provider}:${next.id}`,
						},
					});
					continue;
				}

				// Terminal failure — surface the error to the caller. Emit the
				// system error plus an errored assistant message so callers that
				// inspect messages() for stopReason still see it (original contract).
				pushMsg({
					type: "system",
					subtype: "error",
					content: err.message || "LLM request failed (unknown error)",
					metadata: { model: err.model, provider: err.provider, api: err.api },
				});
				pushMsg({
					type: "assistant",
					content: "",
					model: err.model ?? "unknown",
					provider: err.provider ?? "unknown",
					stopReason: "error",
					errorMessage: err.message,
				});
				return;
			}
		};

		// 10. Send prompt — run inside the session span's context so that
		// gen_ai.chat and gitagent.tool.execute spans become children of
		// gitagent.agent.session.
		if (typeof options.prompt === "string") {
			// Fire pre_query hook before sending to LLM
			if (hooksConfig?.hooks.pre_query) {
				const result = await runHooks(hooksConfig.hooks.pre_query, loaded.agentDir, {
					event: "pre_query",
					session_id: _sessionId,
					prompt: options.prompt,
				});
				if (result.action === "block") {
					pushMsg({
						type: "system",
						subtype: "hook_blocked",
						content: `Query blocked by hook: ${result.reason || "no reason given"}`,
					});
					channel.finish();
					return;
				}
			}
			await runPrompt(options.prompt as string);
		} else {
			// Multi-turn: iterate the async iterable
			for await (const userMsg of options.prompt) {
				pushMsg({ type: "user", content: userMsg.content });
				// Fire pre_query hook for each turn
				if (hooksConfig?.hooks.pre_query) {
					const result = await runHooks(hooksConfig.hooks.pre_query, loaded.agentDir, {
						event: "pre_query",
						session_id: _sessionId,
						prompt: userMsg.content,
					});
					if (result.action === "block") {
						pushMsg({
							type: "system",
							subtype: "hook_blocked",
							content: `Query blocked by hook: ${result.reason || "no reason given"}`,
						});
						channel.finish();
						return;
					}
				}
				await runPrompt(userMsg.content);
			}
		}

		// Emit session_end and close the stream (once).
		finalizeRun();

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
		} finally {
			// Tear down MCP servers on every exit path — success, hook-block
			// early-return, abort, and error (this finally runs before the
			// .catch() handler below). cleanup() is idempotent.
			if (mcpSetup) {
				try { await mcpSetup.cleanup(); } catch { /* best-effort */ }
			}
			// Close the session span on every exit path — success, hook-block
			// early-return, and the .catch() handler below (rethrow so this
			// runs first).
			try {
				_session.end({ "gitagent.cost_usd": _totalCostUsd });
			} catch {
				/* ignore */
			}
		}
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

		costs() {
			return costTracker.get();
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
