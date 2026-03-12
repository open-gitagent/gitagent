# GitClaw Architecture

> Architecture reference for GitClaw — a git-native AI agent runtime (SDK + CLI + Voice UI).
> Based on the actual source code at `src/`.

---

## 1. Overview

GitClaw (v0.3.1) is a TypeScript runtime that loads, configures, and executes AI agents defined as git repositories. It provides:

- **SDK** — programmatic `query()` function that streams agent messages via async generator
- **CLI** — interactive REPL, single-shot mode, and voice mode
- **Voice UI** — HTTP + WebSocket server with a browser-based multimodal interface
- **External tool integrations** — OAuth-based third-party tool injection (Gmail, Slack, etc.)

### Ecosystem Context

GitClaw implements the [GitAgent](https://gitagent.sh) open standard (spec v0.1.0) and optionally integrates with [GitMachine](https://github.com/open-gitagent/gitmachine) for sandboxed execution:

```
GitAgent Spec (agent.yaml, SOUL.md, etc.)
     ↓ loaded by
GitClaw Runtime (SDK, CLI, Voice UI)
     ↓ optionally uses
GitMachine (sandboxed VM + git lifecycle)
```

### Dependencies

| Package | Role |
|---------|------|
| `@sinclair/typebox` | Runtime schema validation for tool parameters |
| `js-yaml` | YAML parsing (agent.yaml, tools, hooks, config) |
| `ws` | WebSocket server for voice mode |
| `gitmachine` *(optional peer dep)* | Sandboxed VM orchestration |

The agent loop and model/provider layer are internal to GitClaw (see [Section 4: Agent Engine Internals](#4-agent-engine-internals)).

---

## 2. Project Structure

### Source Files

```
src/
├── sdk.ts              # Core query() function + tool() factory
├── sdk-types.ts        # All public type definitions
├── sdk-hooks.ts        # Programmatic hook wrapping
├── exports.ts          # Public API surface
├── loader.ts           # Agent loading + system prompt assembly
├── index.ts            # CLI entry point (REPL, single-shot, voice)
├── hooks.ts            # Script-based hook execution
├── tool-loader.ts      # Declarative YAML tool loader
├── skills.ts           # Skill discovery + expansion
├── knowledge.ts        # Knowledge file loading
├── workflows.ts        # Workflow discovery
├── agents.ts           # Sub-agent discovery
├── examples.ts         # Example loading
├── config.ts           # Environment config loading
├── session.ts          # Remote repo clone + branch sessions
├── sandbox.ts          # Sandbox integration
├── compliance.ts       # Compliance validation
├── audit.ts            # JSONL audit logging
├── tools/
│   ├── index.ts        # Built-in tool factory (dispatches local vs sandbox)
│   ├── shared.ts       # Shared tool utilities
│   ├── cli.ts          # Shell command execution
│   ├── read.ts         # File reading
│   ├── write.ts        # File writing
│   ├── memory.ts       # Memory read/write (memory/MEMORY.md)
│   ├── sandbox-cli.ts  # Sandbox variant: shell execution
│   ├── sandbox-read.ts # Sandbox variant: file reading
│   ├── sandbox-write.ts# Sandbox variant: file writing
│   └── sandbox-memory.ts # Sandbox variant: memory
├── voice/
│   ├── server.ts       # HTTP + WebSocket server + integration endpoints
│   ├── adapter.ts      # MultimodalAdapter interface + message types
│   ├── openai-realtime.ts # OpenAI Realtime API adapter
│   ├── gemini-live.ts  # Google Gemini Live adapter
│   ├── index.ts        # Re-exports
│   └── ui.html         # Browser UI (Chat, Files, Integrations tabs)
└── integrations/
    ├── client.ts       # External integration REST API wrapper (native fetch)
    ├── adapter.ts      # Tool conversion + caching
    └── index.ts        # Re-exports
```

### This Repo's Agent Files

GitClaw itself is a GitAgent. These files exist in the repo root:

```
./
├── agent.yaml          # Manifest: spec 0.1.0, 4 tools, 56 max turns
├── SOUL.md             # Identity: "universal git-native agent"
├── RULES.md            # 5 behavioral rules
├── memory/MEMORY.md    # Persistent memory
├── skills/
│   ├── example-skill/  # SKILL.md + scripts/
│   └── gmail-email/    # SKILL.md + scripts/
├── agents/
│   └── assistant/      # Sub-agent (agent.yaml, SOUL.md, RULES.md, memory/)
└── examples/
    ├── sdk-demo.ts     # SDK usage example
    ├── local-repo.ts   # Remote repo session example
    └── debug-events.ts # Event debugging example
```

**Not present in this repo** (but supported by the runtime for any GitAgent):
`DUTIES.md`, `AGENTS.md`, `tools/`, `workflows/`, `knowledge/`, `hooks/`, `config/`, `compliance/`

---

## 3. SDK Core

### 3.1 `query()` — The Main Entry Point

**File:** `src/sdk.ts`

```typescript
function query(options: QueryOptions): Query
```

This is the primary function. It:
1. Validates options (`repo` and `sandbox` are mutually exclusive)
2. Optionally clones a remote repo via `initLocalSession()`
3. Loads the agent via `loadAgent(dir, model, env)`
4. Applies system prompt overrides
5. Creates a sandbox if enabled
6. Builds the tool array (builtin → declarative → SDK-injected → filtered → hook-wrapped)
7. Runs `on_session_start` hooks (script + programmatic)
8. Creates a `pi-agent-core` `Agent` instance with the system prompt, model, and tools
9. Subscribes to agent events and maps them to `GCMessage` via a `Channel`
10. Sends the prompt (single-shot string or multi-turn async iterable)
11. Cleans up (finalize session, stop sandbox)

Returns a `Query` object — an `AsyncGenerator<GCMessage>` with additional methods.

### 3.2 QueryOptions

**File:** `src/sdk-types.ts`

| Property | Type | Description |
|----------|------|-------------|
| `prompt` | `string \| AsyncIterable<GCUserMessage>` | Single-shot or multi-turn input |
| `dir` | `string?` | Working directory (agent root) |
| `model` | `string?` | Model override (`"provider:modelId"`) |
| `env` | `string?` | Environment name for config |
| `systemPrompt` | `string?` | Replace assembled system prompt entirely |
| `systemPromptSuffix` | `string?` | Append to assembled system prompt |
| `tools` | `GCToolDefinition[]?` | Inject additional tools |
| `replaceBuiltinTools` | `boolean?` | Skip the 4 built-in tools |
| `allowedTools` / `disallowedTools` | `string[]?` | Whitelist / blacklist |
| `repo` | `LocalRepoOptions?` | Clone a remote repo as working directory |
| `sandbox` | `SandboxOptions \| boolean?` | Run in sandboxed VM |
| `hooks` | `GCHooks?` | Programmatic hook callbacks |
| `maxTurns` | `number?` | Override max conversation turns |
| `abortController` | `AbortController?` | Cancellation |
| `sessionId` | `string?` | Custom session ID |
| `constraints` | `{ temperature?, maxTokens?, topP?, topK? }?` | Model constraints |

### 3.3 Query Interface

Extends `AsyncGenerator<GCMessage, void, undefined>`:

| Method | Description |
|--------|-------------|
| `abort()` | Cancels execution via AbortController |
| `steer(message)` | Placeholder for mid-conversation steering |
| `sessionId()` | Returns the session UUID |
| `manifest()` | Returns the parsed `AgentManifest` (throws if not yet loaded) |
| `messages()` | Returns a copy of all accumulated messages |

### 3.4 Message Types

`GCMessage` is a discriminated union:

| Type | `type` | Key Fields |
|------|--------|------------|
| `GCAssistantMessage` | `"assistant"` | `content`, `thinking?`, `model`, `provider`, `stopReason`, `errorMessage?`, `usage?` |
| `GCUserMessage` | `"user"` | `content` |
| `GCToolUseMessage` | `"tool_use"` | `toolCallId`, `toolName`, `args` |
| `GCToolResultMessage` | `"tool_result"` | `toolCallId`, `toolName`, `content`, `isError` |
| `GCSystemMessage` | `"system"` | `subtype` (`session_start`, `session_end`, `hook_blocked`, `compliance_warning`, `error`), `metadata?` |
| `GCStreamDelta` | `"delta"` | `deltaType` (`text`, `thinking`), `content` |

`stopReason` values: `"stop"`, `"length"`, `"toolUse"`, `"error"`, `"aborted"`

`usage` fields: `inputTokens`, `outputTokens`, `cacheReadTokens`, `cacheWriteTokens`, `totalTokens`, `costUsd`

### 3.5 `tool()` Factory

```typescript
function tool(
  name: string,
  description: string,
  inputSchema: Record<string, any>,
  handler: (args: any, signal?: AbortSignal) => Promise<string | { text: string; details?: any }>
): GCToolDefinition
```

Creates a `GCToolDefinition` for injection via `QueryOptions.tools`. Internally converted to a `pi-agent-core` `AgentTool` via `toAgentTool()` using `buildTypeboxSchema()`.

### 3.6 Internal Streaming

`createChannel<T>()` implements a backpressure-aware async channel:
- `push(v)` — buffers a value or resolves a waiting pull
- `finish()` — signals the stream is done
- `pull()` — returns a promise that resolves with the next value or `done: true`

Agent events are mapped to `GCMessage` via `agent.subscribe()` and pushed into the channel. The `Query` object's `next()` calls `channel.pull()`.

---

## 4. Agent Engine Internals

GitClaw's agent loop, model abstraction, and streaming infrastructure power the core execution. This section documents how they work under the hood.

### 4.1 Architecture Layers

```
┌──────────────────────────────────────────────────────┐
│  GitClaw SDK (query, tool, hooks, sessions)          │
├──────────────────────────────────────────────────────┤
│  Agent Engine (Agent class, agent loop, events)      │
├──────────────────────────────────────────────────────┤
│  Model Layer (providers, streaming, cost tracking)   │
├──────────────────────────────────────────────────────┤
│  LLM APIs (Anthropic, OpenAI, Google, Bedrock, etc.) │
└──────────────────────────────────────────────────────┘
```

### 4.2 Agent Class

The `Agent` class manages state, runs the agent loop, and emits events.

**Construction** (in `sdk.ts`):
```typescript
const agent = new Agent({
  initialState: {
    systemPrompt,
    model: loaded.model,
    tools,
    ...modelOptions,   // temperature, maxTokens, etc.
  },
});
```

**AgentState** — the immutable internal state:

| Field | Type | Description |
|-------|------|-------------|
| `systemPrompt` | `string` | Full system prompt |
| `model` | `Model<any>` | Resolved model instance |
| `thinkingLevel` | `ThinkingLevel` | `"off"` \| `"minimal"` \| `"low"` \| `"medium"` \| `"high"` \| `"xhigh"` |
| `tools` | `AgentTool<any>[]` | Available tools |
| `messages` | `AgentMessage[]` | Conversation history |
| `isStreaming` | `boolean` | Currently generating |
| `streamMessage` | `AgentMessage \| null` | Partial message during streaming |
| `pendingToolCalls` | `Set<string>` | In-flight tool call IDs |
| `error` | `string?` | Last error |

**Key methods:**

| Method | Purpose |
|--------|---------|
| `prompt(message)` | Send user message, start agent loop |
| `continue()` | Resume from current context (retry after error) |
| `subscribe(fn)` | Register event listener; returns unsubscribe function |
| `steer(message)` | Queue mid-execution interrupt message |
| `followUp(message)` | Queue post-completion continuation |
| `abort()` | Cancel current operation |
| `waitForIdle()` | Wait for agent to finish |
| `setSystemPrompt(v)` | Update system prompt |
| `setModel(m)` | Change model |
| `setTools(t)` | Replace tool set |
| `replaceMessages(ms)` | Replace conversation history |

### 4.3 Agent Loop

The agent loop is the core execution cycle. It runs until the model stops generating tool calls.

```
┌─────────────────────────────────────────────────────────┐
│ OUTER LOOP: Follow-up messages                          │
│                                                         │
│  ┌───────────────────────────────────────────────────┐  │
│  │ INNER LOOP: Tool calls + steering                 │  │
│  │                                                   │  │
│  │  1. Check for STEERING messages (mid-execution)   │  │
│  │     ↓                                             │  │
│  │  2. Inject pending messages into context           │  │
│  │     ↓                                             │  │
│  │  3. Stream ASSISTANT RESPONSE from LLM            │  │
│  │     a. transformContext() — optional pruning       │  │
│  │     b. convertToLlm() — filter to LLM messages    │  │
│  │     c. Stream via model provider                   │  │
│  │     d. Emit message_start / message_update / end   │  │
│  │     ↓                                             │  │
│  │  4. Check for TOOL CALLS in response              │  │
│  │     ↓                                             │  │
│  │  5. EXECUTE TOOLS (sequentially)                  │  │
│  │     a. Emit tool_execution_start                   │  │
│  │     b. tool.execute(toolCallId, args, signal)     │  │
│  │     c. Emit tool_execution_end                     │  │
│  │     d. Check for steering → skip remaining tools   │  │
│  │     ↓                                             │  │
│  │  6. Create tool result messages                    │  │
│  │     ↓                                             │  │
│  │  7. Emit turn_end                                  │  │
│  │     ↓                                             │  │
│  │  8. Loop if: more tool calls OR steering queued    │  │
│  └───────────────────────────────────────────────────┘  │
│                                                         │
│  9. Check for FOLLOW-UP messages                        │
│     → If found, loop back to inner loop                 │
│     → Otherwise, emit agent_end and exit                │
└─────────────────────────────────────────────────────────┘
```

**`convertToLlm()`** (default): Filters `AgentMessage[]` to only `user`, `assistant`, and `toolResult` messages — stripping any custom message types.

**`transformContext()`** (optional): Hook for pruning old messages, injecting context, or compressing history before sending to the LLM. Called every turn.

### 4.4 Agent Events

Events emitted by the agent loop, consumed by `sdk.ts` via `agent.subscribe()`:

| Event | When | Key Fields |
|-------|------|------------|
| `agent_start` | Loop begins | — |
| `agent_end` | Loop finishes | `messages: AgentMessage[]` |
| `turn_start` | New LLM turn begins | — |
| `turn_end` | Turn completes | `message`, `toolResults` |
| `message_start` | LLM begins generating | `message` (partial) |
| `message_update` | Streaming delta | `message`, `assistantMessageEvent` |
| `message_end` | LLM stops generating | `message` (complete) |
| `tool_execution_start` | Tool begins | `toolCallId`, `toolName`, `args` |
| `tool_execution_update` | Tool streams progress | `toolCallId`, `partialResult` |
| `tool_execution_end` | Tool completes | `toolCallId`, `toolName`, `result`, `isError` |

**Event mapping in `sdk.ts`:**
- `message_update` with `text_delta` → `GCStreamDelta { deltaType: "text" }`
- `message_update` with `thinking_delta` → `GCStreamDelta { deltaType: "thinking" }`
- `message_end` (assistant) → `GCAssistantMessage` with accumulated text/thinking
- `tool_execution_start` → `GCToolUseMessage`
- `tool_execution_end` → `GCToolResultMessage`
- `agent_start` → `GCSystemMessage { subtype: "session_start" }`
- `agent_end` → `GCSystemMessage { subtype: "session_end" }`

### 4.5 AgentTool Interface

Tools registered with the agent must implement:

```typescript
interface AgentTool<TParameters extends TSchema> {
  name: string;
  label: string;                    // Display name
  description: string;
  parameters: TParameters;          // Typebox schema

  execute(
    toolCallId: string,
    params: Static<TParameters>,
    signal?: AbortSignal,
    onUpdate?: (partialResult) => void
  ): Promise<{
    content: (TextContent | ImageContent)[];
    details: any;
  }>;
}
```

GitClaw's `toAgentTool()` in `sdk.ts` converts `GCToolDefinition` (the public interface) to `AgentTool` by wrapping the handler and converting the input schema via `buildTypeboxSchema()`.

### 4.6 Model & Provider Layer

The model layer abstracts LLM providers behind a uniform interface.

**Model definition:**

```typescript
interface Model<TApi extends Api> {
  id: string;                         // e.g., "claude-sonnet-4-5-20250929"
  name: string;                       // Human-readable name
  api: TApi;                          // API type identifier
  provider: Provider;                 // e.g., "anthropic", "openai", "google"
  baseUrl: string;                    // API endpoint
  reasoning: boolean;                 // Supports extended thinking
  input: ("text" | "image")[];       // Supported input modalities
  cost: {                             // Per 1M tokens
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
  };
  contextWindow: number;
  maxTokens: number;
}
```

**Model resolution** (in `loader.ts`): `parseModelString("anthropic:claude-sonnet-4-5-20250929")` → calls `getModel("anthropic", "claude-sonnet-4-5-20250929")` → returns a `Model` instance with the provider's base URL, costs, and capabilities.

**Registered providers:**

| API Identifier | Provider | SDK |
|----------------|----------|-----|
| `anthropic-messages` | Anthropic | `@anthropic-ai/sdk` |
| `openai-completions` | OpenAI (+ compatible: Groq, xAI, DeepSeek, Mistral, OpenRouter) | `openai` SDK |
| `openai-responses` | OpenAI Responses API | `openai` SDK |
| `google-generative-ai` | Google Gemini | `@google/genai` |
| `google-vertex` | Google Vertex AI | `@google/genai` |
| `bedrock-converse-stream` | AWS Bedrock | AWS SDK |
| `azure-openai-responses` | Azure OpenAI | `openai` SDK |

### 4.7 Streaming Pipeline

When the agent loop needs an LLM response, this pipeline executes:

```
AgentMessage[] (conversation history)
    ↓
transformContext()         — optional pruning/injection
    ↓
convertToLlm()           — filter to user/assistant/toolResult
    ↓
Provider stream function  — HTTP/SSE to LLM API
    ↓
AssistantMessageEvent stream:
  start → text_start → text_delta* → text_end
        → thinking_start → thinking_delta* → thinking_end
        → toolcall_start → toolcall_delta* → toolcall_end
  done | error
    ↓
AssistantMessage (final result)
```

**AssistantMessage** — the LLM response object:

| Field | Type | Description |
|-------|------|-------------|
| `role` | `"assistant"` | Always assistant |
| `content` | `(TextContent \| ThinkingContent \| ToolCall)[]` | Response blocks |
| `api` | `Api` | Which API was used |
| `provider` | `Provider` | Which provider |
| `model` | `string` | Model ID |
| `usage` | `Usage` | Token counts and costs |
| `stopReason` | `StopReason` | `"stop"` \| `"length"` \| `"toolUse"` \| `"error"` \| `"aborted"` |
| `errorMessage` | `string?` | If stopReason is "error" |
| `timestamp` | `number` | Unix ms |

**Content block types:**

| Type | Fields |
|------|--------|
| `TextContent` | `text`, `textSignature?` |
| `ThinkingContent` | `thinking`, `thinkingSignature?`, `redacted?` |
| `ToolCall` | `id`, `name`, `arguments` |

### 4.8 Cost & Usage Tracking

Every LLM call returns a `Usage` object:

```typescript
interface Usage {
  input: number;           // Input tokens
  output: number;          // Output tokens
  cacheRead: number;       // Cache read tokens (Anthropic prompt caching)
  cacheWrite: number;      // Cache write tokens
  totalTokens: number;     // Sum
  cost: {
    input: number;         // USD
    output: number;        // USD
    cacheRead: number;     // USD
    cacheWrite: number;    // USD
    total: number;         // USD
  };
}
```

Cost calculation: `cost_component = (model.cost.component / 1_000_000) * usage.component`

GitClaw maps this to `GCAssistantMessage.usage` with fields: `inputTokens`, `outputTokens`, `cacheReadTokens`, `cacheWriteTokens`, `totalTokens`, `costUsd`.

### 4.9 Provider-Specific Features

**Anthropic:** Extended thinking with `thinkingLevel` or adaptive `effort` (low/medium/high/max for Opus/Sonnet 4.6). Prompt caching with configurable retention (`"short"` ephemeral, `"long"` 1-hour TTL). Fine-grained tool streaming beta.

**OpenAI:** Reasoning effort mapping (`minimal` → `xhigh`). OpenAI-compatible endpoints auto-detected (Groq, DeepSeek, xAI, etc. via provider name/baseUrl). Strict mode for tool schemas.

**Google:** Native thinking support for Gemini 3.0 models. Budget tokens for older models. Continuous streaming via WebSocket for Gemini Live (used by voice adapter).

### 4.10 Extension Points

| Hook | Purpose | Used By |
|------|---------|---------|
| `transformContext()` | Prune/inject messages before LLM call | Context compression, knowledge injection |
| `convertToLlm()` | Filter custom message types | Default strips non-LLM messages |
| `getSteeringMessages()` | Interrupt mid-tool-execution | `agent.steer()` |
| `getFollowUpMessages()` | Continue after natural stop | `agent.followUp()` |
| `getApiKey(provider)` | Dynamic API key resolution | Token refresh, per-provider keys |
| Custom `streamFn` | Override LLM communication | Proxy backends |

---

## 5. Agent Loading

**File:** `src/loader.ts`

```typescript
function loadAgent(agentDir: string, modelFlag?: string, envFlag?: string): Promise<LoadedAgent>
```

### 5.1 LoadedAgent

| Field | Type | Description |
|-------|------|-------------|
| `systemPrompt` | `string` | Fully assembled prompt |
| `manifest` | `AgentManifest` | Parsed `agent.yaml` |
| `model` | `Model<any>` | Resolved model instance (see [4.6](#46-model--provider-layer)) |
| `skills` | `SkillMetadata[]` | From `skills/` |
| `knowledge` | `LoadedKnowledge` | From `knowledge/index.yaml` |
| `workflows` | `WorkflowMetadata[]` | From `workflows/` |
| `subAgents` | `SubAgentMetadata[]` | From `agents/` |
| `examples` | `ExampleEntry[]` | From `examples/` |
| `envConfig` | `EnvConfig` | From `config/` |
| `sessionId` | `string` | Generated UUID |
| `agentDir` | `string` | Agent root path |
| `gitagentDir` | `string` | `.gitagent/` state path |
| `complianceWarnings` | `ComplianceWarning[]` | Validation results |

### 5.2 System Prompt Assembly

The system prompt is built by concatenating (in order):

1. Agent name, version, description header
2. `SOUL.md` — identity and personality
3. `RULES.md` — behavioral constraints
4. Parent `RULES.md` (if `extends:` is set, inherited rules are merged)
5. `DUTIES.md` — ongoing responsibilities
6. `AGENTS.md` — sub-agent descriptions
7. Memory instructions
8. Knowledge blocks (always-load entries from `knowledge/index.yaml`)
9. Skills blocks (discovered from `skills/`)
10. Workflows blocks (discovered from `workflows/`)
11. Sub-agents blocks (discovered from `agents/`)
12. Examples blocks (loaded from `examples/`)
13. Compliance context (from `compliance/`)

Each step reads files if they exist and silently skips if not — all are optional except `agent.yaml`.

### 5.3 Model Resolution

Priority chain:
```
envConfig.model_override  >  CLI --model flag  >  manifest.model.preferred  >  error
```

Model strings use `"provider:modelId"` format (e.g., `"anthropic:claude-sonnet-4-5-20250929"`). Parsed by `parseModelString()`.

### 5.4 Inheritance & Dependencies

**`extends:`** — Clones parent agent repo into `.gitagent/`, deep-merges manifests (child wins), unions tool/skill arrays, includes parent RULES.md.

**`dependencies:`** — Clones external repos into `.gitagent/deps/` using git tags.

---

## 6. Tool System

### 6.1 Built-in Tools

**File:** `src/tools/index.ts`

```typescript
function createBuiltinTools(config: BuiltinToolsConfig): AgentTool<any>[]
```

| Tool | Implementation | Sandbox Variant |
|------|---------------|-----------------|
| `cli` | `tools/cli.ts` — spawns shell commands | `tools/sandbox-cli.ts` — `machine.execute()` |
| `read` | `tools/read.ts` — reads local files | `tools/sandbox-read.ts` — `machine.readFile()` |
| `write` | `tools/write.ts` — writes local files | `tools/sandbox-write.ts` — `machine.writeFile()` |
| `memory` | `tools/memory.ts` — reads/writes `memory/MEMORY.md` | `tools/sandbox-memory.ts` |

If `SandboxContext` is provided, the sandbox variants are used instead.

### 6.2 Declarative YAML Tools

**File:** `src/tool-loader.ts`

```typescript
function loadDeclarativeTools(agentDir: string): Promise<AgentTool<any>[]>
```

Scans `{agentDir}/tools/` for `.yaml`/`.yml` files. Each defines:

```yaml
name: tool-name
description: What it does
input_schema:
  type: object
  properties:
    param: { type: string, description: "..." }
  required: [param]
implementation:
  script: scripts/tool.sh
  runtime: sh               # default
```

Execution: spawns `sh <script>` with JSON args on stdin, captures stdout. 120-second timeout. `buildTypeboxSchema()` converts the input schema to Typebox for validation.

### 6.3 Tool Registration Flow in `query()`

```
1. createBuiltinTools(dir, timeout, sandbox?)     → [cli, read, write, memory]
2. loadDeclarativeTools(agentDir)                  → tools/*.yaml
3. options.tools.map(toAgentTool)                  → SDK-injected tools
4. Filter: allowedTools / disallowedTools          → whitelist/blacklist
5. wrapToolWithHooks(tool, hooksConfig, ...)       → script-based hook wrapping
6. wrapToolWithProgrammaticHooks(tool, hooks, ...) → programmatic hook wrapping
```

---

## 7. Hooks & Lifecycle

Two complementary systems — script-based (`src/hooks.ts`) and programmatic (`src/sdk-hooks.ts`) — implementing the same event model.

### 7.1 Events

| Script Event | Programmatic Event | Trigger | Can Block? |
|--------------|--------------------|---------|------------|
| `on_session_start` | `onSessionStart` | Agent session begins | Yes |
| `pre_tool_use` | `preToolUse` | Before each tool execution | Yes |
| `post_response` | `postResponse` | After assistant message | No |
| `on_error` | `onError` | On error | No |

### 7.2 Actions

| Action | Effect |
|--------|--------|
| `allow` | Proceed normally (default if hook returns nothing) |
| `block` | Stop tool execution; return reason as system message |
| `modify` | Replace tool args with modified version, then execute |

### 7.3 Script-Based Hooks

Configured in `hooks/hooks.yaml`:

```yaml
hooks:
  pre_tool_use:
    - script: hooks/validate.sh
      description: Validate tool args
```

- Executed via `spawn("sh", [scriptPath])` with JSON on stdin
- Must output JSON `{action, reason?, args?}`
- 10-second timeout per hook
- Sequential; stops on first block/modify

### 7.4 Programmatic Hooks

Passed via `QueryOptions.hooks`:

```typescript
interface GCHooks {
  onSessionStart?: (ctx: GCHookContext) => Promise<GCHookResult> | GCHookResult;
  preToolUse?: (ctx: GCPreToolUseContext) => Promise<GCHookResult> | GCHookResult;
  postResponse?: (ctx: GCHookContext) => Promise<void> | void;
  onError?: (ctx: GCHookContext & { error: string }) => Promise<void> | void;
}
```

`GCPreToolUseContext` adds `toolName` and `args` to the base context (`sessionId`, `agentName`, `event`).

Both systems wrap tools identically: intercept `execute()` → run hook → allow/block/modify → proceed or halt.

---

## 8. Voice & Multimodal

**Files:** `src/voice/server.ts`, `src/voice/adapter.ts`, `src/voice/openai-realtime.ts`, `src/voice/gemini-live.ts`, `src/voice/ui.html`

### 8.1 Server

```typescript
function startVoiceServer(opts: VoiceServerOptions): Promise<() => Promise<void>>
```

HTTP server (default port 3333) with WebSocket upgrade. Returns a shutdown function.

**HTTP routes:**

| Route | Purpose |
|-------|---------|
| `GET /` | Serves `ui.html` |
| `GET /health` | Health check |
| `GET /api/files?path=` | Directory tree listing (filters hidden dirs) |
| `GET /api/file?path=` | Read file (max 1MB, safe path validation) |
| `PUT /api/file` | Write file content |
| `GET /api/integrations/toolkits` | List available toolkits |
| `POST /api/integrations/connect` | Initiate OAuth flow |
| `GET /api/integrations/connections` | List active connections |
| `DELETE /api/integrations/connections/{id}` | Disconnect a toolkit |
| `GET /api/integrations/callback` | OAuth callback (redirects popup) |

### 8.2 Adapter Pattern

```typescript
interface MultimodalAdapter {
  connect(opts: { toolHandler, onMessage }): Promise<void>;
  send(msg: ClientMessage): void;
  disconnect(): Promise<void>;
}
```

| Backend | File | Provider | Audio | Video |
|---------|------|----------|-------|-------|
| `openai-realtime` | `openai-realtime.ts` | OpenAI gpt-4o-realtime via WSS | Native (24kHz) | Frame injection per turn |
| `gemini-live` | `gemini-live.ts` | Gemini 2.5 Flash via WSS | 24kHz↔16kHz resampling | Continuous streaming |

**OpenAI adapter:** Uses a single `run_agent` function tool. Voice activity detection with 800ms silence threshold. Video frames stored and injected as images on next user turn.

**Gemini adapter:** Native audio resampling (linear interpolation). Supports continuous video frames. Context compression at 25K tokens (sliding window to 12.5K).

### 8.3 WebSocket Protocol

**Client → Server:**

| `type` | Fields |
|--------|--------|
| `audio` | `audio: base64` (PCM 16-bit, 24kHz) |
| `video_frame` | `frame: base64, mimeType: "image/jpeg"` (every 1000ms) |
| `text` | `text: string` |

**Server → Client:**

| `type` | Fields |
|--------|--------|
| `audio_delta` | `audio: base64` |
| `transcript` | `role: "user"\|"assistant"`, `text`, `partial?` |
| `agent_working` | `query` |
| `agent_done` | — |
| `tool_call` | `toolName`, `args` |
| `tool_result` | `toolName`, `content`, `isError` |
| `agent_thinking` | `text` |
| `error` | `message` |

### 8.4 Web UI

`src/voice/ui.html` — single-file browser application with three tabs:

- **Chat** — camera preview (canvas frame capture), mic/camera toggles, conversation display with tool call rendering, text input
- **Files** — file tree sidebar (260px), multi-tab Monaco editor, keyboard shortcuts (Cmd+S save, Cmd+W close)
- **Integrations** — Toolkit grid with connect/disconnect buttons, OAuth popup flow (600x700)

---

## 9. External Tool Integrations

**Files:** `src/integrations/client.ts`, `src/integrations/adapter.ts`

### 9.1 Integration Client

Native `fetch` wrapper around external tool provider APIs. Auth via API key header.

| Method | Purpose |
|--------|---------|
| `listToolkits(userId?)` | Available toolkits with connection status |
| `searchTools(query, slugs?, limit)` | Semantic tool search |
| `listTools(toolkitSlug)` | All tools in a toolkit |
| `getOrCreateAuthConfig(toolkitSlug)` | Get/create auth config (cached) |
| `initiateConnection(toolkit, userId, redirectUrl?)` | Start OAuth — 2 steps: auth config → connected account |
| `listConnections(userId)` | Active connections |
| `deleteConnection(id)` | Revoke connection |
| `executeTool(toolSlug, userId, params, connectedAccountId?)` | Execute an action |

### 9.2 Integration Adapter

Converts external tools to `GCToolDefinition[]`:

- **Naming:** `{provider}_{toolkitSlug}_{slug}` (sanitized to alphanumeric + underscore)
- **Description:** `[{Provider}/{toolkit}] {description}` with action hints — direct-action tools get "USE THIS to perform directly", draft tools get "Only use when user explicitly asks"
- **Caching:** 30-second TTL on tool lists, cache invalidated on disconnect
- **Sorting:** Direct-action tools (SEND, CREATE, LIST) ranked above draft tools
- **Execution:** Handler calls `executeTool()` and returns stringified result

### 9.3 OAuth Flow

```
1. getOrCreateAuthConfig(toolkitSlug) → auth_config_id  (cached per toolkit)
2. initiateConnection(toolkit, userId, redirectUrl) → redirectUrl
3. UI opens popup (600x700) → user authenticates with provider
4. Provider redirects to /api/integrations/callback → HTML page posts message to opener
5. UI receives postMessage or focus event → refreshes toolkit state
```

### 9.4 Tool Injection at Query Time

In `server.ts`, the `createToolHandler()` function:
1. Gets connected toolkit slugs from the adapter
2. Runs `searchTools(userQuery, slugs)` for semantic matching (fallback: `getTools()` for all)
3. Filters to prefer direct-action tools over drafts
4. Appends a system prompt suffix telling the agent about available integrations
5. Calls `query()` with the integration tools injected

---

## 10. Supporting Systems

### 10.1 Skills (`src/skills.ts`)

- Discovered from `skills/` — each skill is a subdirectory with `SKILL.md`
- Kebab-case directory names enforced
- `SKILL.md` parsed for YAML frontmatter (`name`, `description`) and body instructions
- `discoverSkills()` → `loadSkill()` → `formatSkillsForPrompt()`
- CLI supports `/skill:name` expansion via `expandSkillCommand()`

### 10.2 Knowledge (`src/knowledge.ts`)

- Configured via `knowledge/index.yaml`
- Each entry: `path`, `tags[]`, `priority` (high/medium/low), `always_load?`
- `always_load: true` entries injected into system prompt; others available on-demand
- Returns `LoadedKnowledge` with `preloaded` and `available` arrays

### 10.3 Workflows (`src/workflows.ts`)

- Discovered from `workflows/` directory
- Supports `.yaml`/`.yml` and `.md` formats
- `WorkflowMetadata`: `name`, `description`, `filePath`, `format`

### 10.4 Sub-Agents (`src/agents.ts`)

- Discovered from `agents/` directory
- **Directory form:** subdirectory with its own `agent.yaml` (e.g., `agents/assistant/`)
- **File form:** single `.md` file with description
- `SubAgentMetadata`: `name`, `description`, `type` ("directory" | "file"), `path`
- Formatted with delegation instructions in system prompt

### 10.5 Examples (`src/examples.ts`)

- Loaded from `examples/*.md` (only .md files)
- `ExampleEntry`: `name`, `content`
- Injected into system prompt for in-context calibration

### 10.6 Config (`src/config.ts`)

- Loads `config/default.yaml` then deep-merges `config/{env}.yaml`
- Environment from `--env` flag or `GITCLAW_ENV` env var
- `EnvConfig`: `log_level?`, `model_override?`, plus arbitrary keys

### 10.7 Sessions (`src/session.ts`)

```typescript
function initLocalSession(opts: LocalRepoOptions): LocalSession
```

- Clones remote repo (injects PAT into URL), creates/resumes session branch
- Auto-scaffolds `agent.yaml`, `memory/`, `SOUL.md` if missing
- `LocalSession` methods: `commitChanges(msg?)`, `push()`, `finalize()`
- Sessions are git branches — natural isolation, full history, merge via PR

### 10.8 Compliance (`src/compliance.ts`)

- `validateCompliance(manifest)` → `ComplianceWarning[]` with `rule`, `message`, `severity` (error/warning)
- Rules: high/critical risk require `human_in_the_loop: true`; critical requires `audit_logging: true`
- `loadComplianceContext(agentDir)` reads regulatory maps and validation schedules from `compliance/`
- Supported frameworks: FINRA, SEC, Federal Reserve, CFPB

### 10.9 Audit (`src/audit.ts`)

- `AuditLogger` class writes JSONL to `.gitagent/audit/`
- `AuditEntry`: `timestamp`, `session_id`, `event`, `tool?`, `args?`, `result?`, `error?`
- Methods: `logSessionStart()`, `logToolUse()`, `logToolResult()`, `logResponse()`, `logError()`, `logSessionEnd()`
- Enabled when `compliance.recordkeeping.audit_logging === true`

### 10.10 Sandbox (`src/sandbox.ts`)

```typescript
function createSandboxContext(config: SandboxConfig, dir: string): Promise<SandboxContext>
```

- Dynamically imports `gitmachine` (optional peer dep)
- Creates sandboxed VM via GitMachine
- Returns `SandboxContext`: `{ gitMachine, machine, repoPath }`
- `detectRepoUrl(dir)` auto-detects git remote origin

---

## 11. CLI

**File:** `src/index.ts`

```
gitclaw [options]

--model <provider:model>   Override model
--dir <path>               Working directory (default: cwd)
--prompt <text>            Single-shot mode
--env <name>               Environment name
--sandbox                  Enable sandboxed execution
--repo <url>               Remote repo URL
--pat <token>              Personal access token for --repo
--session <id>             Resume a session
--voice <openai|gemini>    Start voice server
```

**Modes:**

| Mode | Trigger | Behavior |
|------|---------|----------|
| Voice | `--voice openai\|gemini` | Starts HTTP/WS server, opens browser UI |
| Single-shot | `--prompt "..."` | Runs once, prints output, exits |
| REPL | (default) | Interactive loop with readline |

**REPL commands:** `/quit`, `/memory` (show memory), `/skills` (list skills), `/skill:name` (expand skill)

**Auto-scaffolding:** If `--dir` points to a non-GitAgent directory, the CLI creates `agent.yaml`, `SOUL.md`, and `memory/` automatically via `ensureRepo()`.

---

## 12. Public API

**File:** `src/exports.ts`

**Functions:**

| Export | Source | Purpose |
|--------|--------|---------|
| `query` | `sdk.ts` | Run agent, stream messages |
| `tool` | `sdk.ts` | Create tool definitions |
| `loadAgent` | `loader.ts` | Load agent config without running |
| `createSandboxContext` | `sandbox.ts` | Create sandboxed VM context |
| `initLocalSession` | `session.ts` | Clone repo + create session branch |
| `startVoiceServer` | `voice/server.ts` | Start voice HTTP/WS server |

**Type exports:** `Query`, `QueryOptions`, `LocalRepoOptions`, `SandboxOptions`, `GCMessage` (+ all 6 subtypes), `GCToolDefinition`, `GCHooks`, `GCHookResult`, `GCPreToolUseContext`, `GCHookContext`, `AgentManifest`, `LoadedAgent`, `SkillMetadata`, `WorkflowMetadata`, `SubAgentMetadata`, `ComplianceWarning`, `EnvConfig`, `SandboxConfig`, `SandboxContext`, `LocalSession`, `VoiceAdapter`, `VoiceAdapterConfig`, `VoiceServerOptions`

Package entry point: `dist/exports.js` (ESM). CLI binary: `dist/index.js`.

---

## 13. GitMachine Integration

GitMachine is an **optional peer dependency** (`gitmachine >= 0.1.0`). It provides sandboxed VM orchestration with git lifecycle management.

### Architecture

```
GitMachine           ← Git lifecycle (clone, commit, push)
  └── Machine        ← Abstract VM interface
       └── <Provider> ← Concrete implementation (pluggable VM backend)
```

### Lifecycle

| Phase | Actions |
|-------|---------|
| **Start** | Start VM → clone repo → checkout session branch → configure git → onStart hook |
| **Pause** | Auto-commit changes → onPause hook → pause VM |
| **Resume** | Resume VM → onResume hook |
| **Stop** | Auto-commit → push to remote → onEnd hook → stop VM |

### Machine Interface

```typescript
interface Machine {
  start(): Promise<void>;
  pause(): Promise<void>;
  resume(): Promise<void>;
  stop(): Promise<void>;
  execute(command: string, opts?): Promise<ExecutionResult>;
  readFile(path: string): Promise<string>;
  writeFile(path: string, content: string): Promise<void>;
}
```

**Sessions = branches.** Each sandbox session maps to a git branch. `GitMachine.connect(sandboxId)` reconnects to a running sandbox.

When sandbox mode is active in GitClaw, the built-in tools delegate to the Machine interface instead of local filesystem/shell.

---

## 14. Data Flow

### 14.1 Query Lifecycle

```
prompt
  │
  ▼
query(options)
  ├─ initLocalSession()          [if --repo]
  ├─ loadAgent(dir, model, env)
  │    ├─ parse agent.yaml
  │    ├─ resolve model
  │    ├─ resolve extends/deps
  │    ├─ assemble system prompt
  │    └─ validate compliance
  ├─ createSandboxContext()       [if --sandbox]
  ├─ build tools
  │    ├─ createBuiltinTools()
  │    ├─ loadDeclarativeTools()
  │    ├─ toAgentTool(options.tools)
  │    ├─ filter (allow/disallow)
  │    └─ wrap with hooks
  ├─ run on_session_start hooks
  ├─ new Agent({ systemPrompt, model, tools })
  ├─ agent.subscribe() → map events → channel.push(GCMessage)
  ├─ agent.prompt(text)
  │    ├─ assistant response → GCAssistantMessage
  │    ├─ tool_use → execute → tool_result → loop
  │    └─ stop → agent_end
  └─ cleanup (finalize session, stop sandbox)
       │
       ▼
  yield GCMessage via AsyncGenerator
```

### 14.2 Voice Flow

```
Browser (ui.html)
  ├─ WebSocket connect
  ├─ audio/video/text ────────────► Voice Server
  │                                   ├─ adapter.send(ClientMessage)
  │                                   │    └─ OpenAI WSS / Gemini WSS
  │                                   ├─ toolHandler invoked
  │                                   │    ├─ integration semantic search
  │                                   │    ├─ query(prompt, tools)
  │                                   │    └─ stream tool_call/result/text
  │  ◄──────────────────────────── adapter.onMessage(ServerMessage)
  └─ render: transcript, tool calls, audio playback
```

### 14.3 Integration Flow

```
Integrations tab
  ├─ GET /api/integrations/toolkits → list available
  ├─ POST /api/integrations/connect → OAuth popup → callback
  │
  └─ At query time:
       searchTools(query) → toGCTool() → inject into query()
       → agent calls {provider}_{toolkit}_{action} → executeTool() → API
```

---

## 15. Configuration Reference

### agent.yaml

See `AgentManifest` in `src/loader.ts`. Required fields: `spec_version`, `name`, `version`, `description`, `model`, `tools`, `runtime`.

### Environment Variables

| Variable | Used By |
|----------|---------|
| `INTEGRATION_API_KEY` | `integrations/client.ts` — external tool provider API auth |
| `ANTHROPIC_API_KEY` | Anthropic provider |
| `OPENAI_API_KEY` | OpenAI provider; `openai-realtime.ts` — voice |
| `GOOGLE_API_KEY` | Google provider; `gemini-live.ts` — voice |
| `GITCLAW_ENV` | `config.ts` — environment name (alternative to `--env`) |
| `SANDBOX_API_KEY` | `sandbox.ts` — sandbox VM provider auth |
| `GITHUB_TOKEN` / `GIT_TOKEN` | `sdk.ts` — fallback token for `--repo` |

`.env` files in the agent root are supported.

### hooks/hooks.yaml

```yaml
hooks:
  on_session_start:
    - script: hooks/init.sh
      description: Initialize session
  pre_tool_use:
    - script: hooks/validate.sh
  post_response:
    - script: hooks/log.sh
  on_error:
    - script: hooks/alert.sh
```

Scripts receive JSON on stdin, must output JSON `{action, reason?, args?}`. 10-second timeout.

### knowledge/index.yaml

```yaml
- path: knowledge/reference.md
  tags: [api]
  priority: high
  always_load: true
```

### config/{env}.yaml

```yaml
log_level: info
model_override: "anthropic:claude-sonnet-4-5-20250929"
```

Deep-merged onto `config/default.yaml`.

---

## 16. GitClaw vs OpenClaw

Both GitClaw and [OpenClaw](https://github.com/openclaw/openclaw) (formerly Clawdbot/Moltbot, created by Peter Steinberger) are TypeScript AI agent runtimes with personal assistant capabilities, voice support, and multi-provider LLM backends. OpenClaw (247K+ GitHub stars, 47.7K forks) pioneered the personal AI agent space with broad messaging channel support. GitClaw builds on similar ideas but takes a git-native, spec-driven, SDK-first approach — adding formal standards, compliance, sandboxing, and real-time multimodal streaming where OpenClaw relies on ad-hoc conventions.

| Dimension | OpenClaw | GitClaw |
|-----------|----------|---------|
| **Git-Native** | Git-backed memory (optional) | Everything is git: agents, sessions, memory, audit, config |
| **Portability** | Tied to OpenClaw Gateway runtime | Every agent is a git repo — clone, fork, PR, version. Any runtime can load a GitAgent repo; custom GitClaw forks are just `git clone` away |
| **Agent Standard** | Ad-hoc workspace files (SOUL.md, AGENTS.md, IDENTITY.md, TOOLS.md) | GitAgent Spec v0.1.0 — formal, portable open standard |
| **Sandbox / Isolation** | Opt-in per-session containers (512 vulns found in Jan 2026 audit) | GitMachine: stateless VM, sessions = branches, built-in isolation by default |
| **Compliance** | None | FINRA, SEC, Federal Reserve, CFPB, segregation of duties, HITL |
| **Security** | 512 vulns in Jan 2026 audit (8 critical); 41% of community skills contain vulnerabilities | Hook-based security policies, designed for regulated environments from day one |
| **SDK API** | Plugin SDK for Gateway extensions; REST channel for embedding | `query()` AsyncGenerator with backpressure, `abort()`, `steer()` |
| **Streaming** | Gateway routes messages to channels (no programmatic streaming API) | First-class AsyncGenerator streaming (GCMessage protocol, 6 message types + deltas) |
| **Voice** | TTS (ElevenLabs/OpenAI/edge), STT (Whisper), Talk Mode with wake words | Real-time multimodal streaming (OpenAI Realtime, Gemini Live), browser UI with camera + mic |
| **Video** | No native video | Continuous video frames (Gemini), per-turn image injection (OpenAI) |
| **Audio** | ElevenLabs TTS + OpenAI Whisper STT via SAG module | Native 24 kHz PCM streaming, VAD, resampling — lower latency, no third-party TTS dependency |
| **Tool Hooks** | No interception layer | `pre_tool_use` hooks (allow / block / modify args before execution) |
| **Audit** | `openclaw security audit` CLI (scans config issues) | Continuous JSONL audit logging (`.gitagent/audit/`) — every action recorded automatically |
| **Sessions** | Gateway-managed, workspace-based | Git branches (natural isolation, full history, merge via PR) |
| **Agent Inheritance** | No | `extends:` → clone parent → deep-merge overlays |
| **Agent Dependencies** | No | `dependencies:` → clone via git tags, composable agents |
| **Cost Tracking** | No | Per-message token + USD cost tracking across all providers |
| **Thinking / Reasoning** | Basic model support | Extended thinking (Anthropic adaptive effort), cross-provider reasoning effort mapping |
| **Philosophy** | Personal AI assistant via messaging channels | Personal + developer agent platform, git-native by design |
| **Architecture** | Gateway (WebSocket) → Agent Runtime → LLM | SDK → Agent Loader → Agent Engine → Model/Provider Layer |
| **Agent Definition** | SOUL.md + scattered workspace config | `agent.yaml` manifest + convention directories |
| **Developer Integration** | Extendable via Plugins (TS/JS) and Webhooks | Embeddable SDK (`npm install gitclaw`) — drop into any app |
| **Entry Points** | CLI + 20+ messaging channels | SDK + CLI + Voice UI + messaging channels (extensible) |
| **Messaging Channels** | 20+ built-in (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Teams, Matrix, LINE…) | Extensible channel architecture — WhatsApp, Slack, etc. via adapters (roadmap) |
| **Tool System** | Browser (Semantic Snapshots), shell, email, calendar, file ops | 6-stage pipeline (builtin → YAML → SDK → filter → hook → agent) — same tool types, more control |
| **Execution Model** | Lane Queue — serial per-session, prevents race conditions | Async agent loop with streaming, cancellation, and backpressure |
| **Web Browsing** | Semantic Snapshots (accessibility tree parsing) | Extensible via tool plugins (browser tools on roadmap) |
| **Model Providers** | 12+ (Anthropic, OpenAI, Google, DeepSeek, Ollama, LM Studio, OpenRouter…) | 7+ (Anthropic, OpenAI, Google, Bedrock, Azure, Vertex) + any OAI-compatible endpoint |
| **Canvas / Visual** | Live Canvas — agent-driven visual workspace (A2UI) | Browser UI with Chat + Files/Monaco editor + Integrations panel |
| **Skills** | 13,700+ community skills on ClawHub (SKILL.md + YAML frontmatter, auto-discovery) | Convention-based `skills/` directory + ClawHub-compatible skill format |
| **Memory** | Local Markdown/YAML under `~/.openclaw/`, grep-searchable, optional git backup | `memory/MEMORY.md`, always git-tracked, tool-accessible, never optional |
| **Configuration** | Gateway config + `~/.openclaw/` workspace | `agent.yaml` + `config/{env}.yaml` deep-merge (environment-aware) |

### Why GitClaw

- **Everything OpenClaw does, plus a formal standard.** GitClaw agents are defined by the GitAgent spec — portable across runtimes, versionable via git tags, reviewable in PRs. OpenClaw's SOUL.md/AGENTS.md conventions only work inside OpenClaw.
- **Embeddable, not just extensible.** GitClaw's `query()` AsyncGenerator drops into any Node.js app with full streaming, cancellation, and backpressure. OpenClaw requires running its Gateway server and connecting via plugins or REST.
- **Compliance and audit built in.** Financial regulatory support (FINRA, SEC, audit logging, segregation of duties) is part of GitClaw's architecture — not an afterthought. OpenClaw has no compliance story.
- **Secure by default.** GitMachine provides stateless VM isolation where sessions are git branches and VMs are disposable. OpenClaw's sandbox is opt-in, and its ecosystem has faced significant security scrutiny.
- **Real-time multimodal, not bolted-on TTS.** GitClaw streams native audio/video via OpenAI Realtime and Gemini Live with VAD and resampling. OpenClaw adds voice via third-party TTS/STT services layered on top of text.
- **Messaging channels are a matter of time, not architecture.** GitClaw's extensible adapter pattern supports adding WhatsApp, Slack, and other channels — the same breadth OpenClaw offers today, on a stronger foundation.

---

## 17. Design Decisions

**Git-native agents.** Every agent is a git repo — versioning, collaboration (PRs), auditability (git log), branching (experiments), and forking (variants) come for free.

**Async generator for `query()`.** Enables real-time streaming, cancellation via `abort()`, and natural backpressure. Consumers process messages as they arrive.

**Hook wrapping over event listeners.** Wrapping tool `execute()` enables blocking and modifying tool calls *before* execution — critical for security policies, compliance, and cost controls.

**Adapter pattern for voice.** OpenAI Realtime and Gemini Live have different protocols, audio formats, and video capabilities. `MultimodalAdapter` abstracts these behind a uniform `connect/send/disconnect` interface.

**Sessions = branches.** Git branches are natural session containers. Each gets isolated state, full history, and can merge back via PR — aligning with human-in-the-loop review.

**Native fetch for integrations.** No SDK dependency — external tool provider APIs are simple enough to wrap directly with `fetch`, keeping the dependency tree small.

**Layered engine architecture.** The agent loop (message → tool call → tool result → loop) and model/provider abstraction are separated into distinct internal layers. GitClaw's SDK subscribes to engine events and maps them to the `GCMessage` protocol, keeping the public API clean while the engine handles LLM streaming, tool execution, steering, and multi-provider support.

**Provider-agnostic model layer.** The model layer uses a registry pattern — each provider (Anthropic, OpenAI, Google, Bedrock, etc.) registers a stream function behind a common interface. Switching providers requires only changing the model string, not any calling code. Cost tracking is built into every provider with per-model pricing data.
