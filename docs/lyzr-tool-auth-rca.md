# RCA: Lyzr Pre-Authorized Tools Not Available in GitAgent

Date: 2026-07-15
Repository: `open-gitagent/gitagent`
Scope: GitAgent + Lyzr tool authorization behavior for Gmail, Slack, and similar ecosystem tools.

## Executive Summary

Users authenticating GitAgent with `LYZR_API_KEY` still need to separately authorize tools such as Gmail and Slack because GitAgent currently uses the Lyzr key only as a model/backend credential. It does not discover, import, proxy, or execute Lyzr ecosystem tools through Lyzr's credential vault.

The current GitAgent implementation executes tools locally through built-in tools, local skills, declarative scripts, SDK-provided tools, or plugins. As a result, any local Gmail or Slack tool must bring its own credentials. The bundled Gmail skill demonstrates this clearly: it sends mail through Gmail SMTP using `GMAIL_USER` and `GMAIL_APP_PASSWORD`, independent of Lyzr.

The recommended way forward is to implement a Lyzr Tool Bridge plugin/provider. This bridge should discover already-authorized Lyzr tools for the current Lyzr user/workspace/agent, register those tools in GitAgent, and forward tool executions to Lyzr server-side. Lyzr would then execute the requested action with credentials already stored in the Lyzr ecosystem.

## Impact

- Users see a duplicated authorization flow for tools they have already authorized in Lyzr.
- GitAgent cannot reliably know which Lyzr ecosystem tools are available.
- GitAgent may select local duplicate skills, such as Gmail SMTP, instead of Lyzr-native OAuth-backed tools.
- Security posture is weaker if users are encouraged to place third-party app passwords or OAuth tokens in local environment files.
- Product experience is inconsistent: the model is Lyzr-backed, but tool execution is not Lyzr-backed.

## Root Cause

### Primary Root Cause

GitAgent does not have a tool-execution integration with Lyzr's authorized connector/tool layer. The Lyzr API key is wired into the model path, not the tool path.

Evidence:

- `examples/lyzr-sdk.ts` reads `LYZR_API_KEY` and maps it into `OPENAI_API_KEY` for OpenAI-compatible model access. See `examples/lyzr-sdk.ts:15-24`.
- The same example configures the model as `lyzr:<agent-id>@https://agent-prod.studio.lyzr.ai/v4`. See `examples/lyzr-sdk.ts:36-38`.
- `src/loader.ts` creates a custom OpenAI-compatible model when the model string contains `@baseUrl`. See `src/loader.ts:81-97` and `src/loader.ts:393-400`.
- `src/loader.ts` uses `LYZR_API_KEY` only as a provider key fallback for custom providers so `pi-ai` can resolve an API key. See `src/loader.ts:406-419`.

### Contributing Cause 1: Tool Execution Is Local by Default

GitAgent builds tools locally and passes them into `pi-agent-core`.

Evidence:

- CLI path builds built-in tools, declarative tools, and plugin tools before creating the `Agent`. See `src/index.ts:532-569` and `src/index.ts:589-596`.
- SDK path does the same with built-ins, declarative tools, plugin tools, and SDK tools. See `src/sdk.ts:176-244` and `src/sdk.ts:301-309`.
- Built-in tools are local filesystem/shell/memory tools. See `src/tools/index.ts:31-58`.
- Declarative tools execute local scripts via `spawn`, passing JSON args through stdin. See `src/tool-loader.ts:50-75` and `src/tool-loader.ts:87-156`.

### Contributing Cause 2: Gmail Skill Uses Independent SMTP Credentials

The bundled Gmail skill is not a Lyzr ecosystem tool. It requires Gmail SMTP credentials and does not use Lyzr authorization state.

Evidence:

- The Gmail skill describes itself as SMTP with App Password authentication. See `skills/gmail-email/SKILL.md:1-4`.
- The setup instructions require `GMAIL_USER` and `GMAIL_APP_PASSWORD`. See `skills/gmail-email/SKILL.md:19-29`.
- The script reads `GMAIL_USER` and `GMAIL_APP_PASSWORD` from environment variables. See `skills/gmail-email/scripts/send_email.py:24-30`.
- Missing local Gmail credentials trigger an error instructing users to set Gmail credentials. See `skills/gmail-email/scripts/send_email.py:31-44`.
- The script connects directly to Gmail SMTP and logs in locally. See `skills/gmail-email/scripts/send_email.py:55-66`.

### Contributing Cause 3: Swagger Defines Lyzr Tool APIs, but GitAgent Does Not Consume Them

The Lyzr Agent API Swagger already exposes tool, credential, MCP, provider, and inference tool-execution endpoints. The gap is not that Lyzr has no tool API surface; the gap is that GitAgent does not call those endpoints to discover and proxy already-authorized tools.

Evidence:

- Plugins can register programmatic tools through `registerTool`. See `src/plugin-sdk.ts:10-25` and `src/plugin-sdk.ts:64-66`.
- Plugin loading collects programmatic tools from plugin entrypoints. See `src/plugins.ts:237-284`.
- GitAgent merges plugin tools into the active tool list. CLI path: `src/index.ts:545-560`; SDK path: `src/sdk.ts:192-207`.
- SDK tools are converted into `AgentTool` objects through `toAgentTool`. See `src/tool-utils.ts:7-27`.
- Lyzr Swagger defines user tool listing at `GET /v3/tools/`.
- Lyzr Swagger defines all-user tool listing at `GET /v3/tools/all/user`.
- Lyzr Swagger defines provider/action listing at `GET /v3/providers/tools/actions/{provider_identifier}` and `GET /v3/providers/tools/all`.
- Lyzr Swagger defines MCP server listing, tool listing, OAuth initiation/status, and execution under `/v3/tools/mcp/*`.
- Lyzr Swagger defines generic tool execution at `POST /v3/inference/tools/execute`.
- Lyzr Swagger defines connected accounts and tool credential management under `/v3/tools/credentials/*`.

## Current Tool Calling Mechanism in GitAgent

### 1. Agent Loading

`loadAgent()` reads the agent manifest, identity files, skills, plugins, workflows, examples, and model configuration. It then returns a composed system prompt, model object, plugin list, and metadata.

Relevant code:

- Manifest parsing: `src/loader.ts:236-250`
- Plugin discovery: `src/loader.ts:263-264`
- Skills discovery and prompt injection: `src/loader.ts:295-307`
- Model resolution: `src/loader.ts:382-419`

### 2. Tool Assembly

GitAgent assembles tools from multiple sources:

- Built-in tools: `cli`, `read`, `write`, `edit`, `memory`, `capture_photo`, `task_tracker`, `skill_learner`
- Declarative tools from `tools/*.yaml`
- Plugin declarative and programmatic tools
- SDK-provided tools in programmatic usage

Relevant code:

- Built-in tool creation: `src/tools/index.ts:31-58`
- CLI tool assembly: `src/index.ts:532-560`
- SDK tool assembly: `src/sdk.ts:176-223`
- Declarative tool loading: `src/tool-loader.ts:161-189`
- Plugin programmatic tool loading: `src/plugins.ts:237-284`

### 3. Hook Wrapping

Tools can be wrapped with hooks before execution. Hooks can block or modify tool calls.

Relevant code:

- Hook config shape: `src/hooks.ts:7-30`
- Hook execution: `src/hooks.ts:44-155`
- Tool wrapper for `pre_tool_use`: `src/hooks.ts:157-198`
- CLI wraps tools with hooks: `src/index.ts:562-569`
- SDK wraps tools with script and programmatic hooks: `src/sdk.ts:225-244`

### 4. Agent Execution

The final `Agent` receives:

- `systemPrompt`
- `model`
- `tools`
- model options such as temperature and token limits

Relevant code:

- CLI creates the agent: `src/index.ts:589-596`
- SDK creates the agent: `src/sdk.ts:301-309`
- CLI sends single-shot prompt: `src/index.ts:638-668`
- SDK sends prompt through `agent.prompt()`: `src/sdk.ts:489-538`

### 5. Tool Call Events

When the model chooses a tool, `pi-agent-core` emits tool execution events. GitAgent subscribes to those events and streams/logs tool calls and results.

Relevant code:

- CLI handles tool start/end events: `src/index.ts:163-177`
- SDK emits `tool_use` messages: `src/sdk.ts:432-440`
- SDK emits `tool_result` messages: `src/sdk.ts:442-450`
- SDK fires failure and file-change hooks after tool results: `src/sdk.ts:452-473`

## Why Lyzr Pre-Authorized Tools Are Not Available Today

The current flow is:

```text
User sets LYZR_API_KEY
  -> GitAgent uses it for Lyzr/OpenAI-compatible model calls
  -> GitAgent locally registers built-in/local/plugin tools
  -> Model may call a local Gmail/Slack tool
  -> Local tool asks for local Gmail/Slack credentials
```

The desired flow is:

```text
User sets LYZR_API_KEY
  -> GitAgent authenticates with Lyzr
  -> GitAgent discovers Lyzr-authorized tools
  -> GitAgent registers those tools locally as proxy tools
  -> Model calls a proxy tool
  -> GitAgent forwards execution to Lyzr
  -> Lyzr executes with stored OAuth credentials
  -> GitAgent returns the result to the model/user
```

The missing component is the bridge between GitAgent's tool registry and Lyzr's server-side tool execution system.

## Proposed Implementation

Implement a `lyzr-tools` GitAgent plugin/provider.

The plugin should:

1. Read configuration from `agent.yaml` plugin config and environment variables.
2. Authenticate to Lyzr with `LYZR_API_KEY`.
3. Discover tools already available to the current Lyzr user/workspace/agent.
4. Register each discovered tool as a GitAgent programmatic tool using `api.registerTool()`.
5. Execute tool calls by proxying them to Lyzr.
6. Return structured auth-required errors when a tool is unavailable or not authorized.
7. Optionally add prompt text telling the model to prefer Lyzr-backed tools over local duplicate skills.

### Proposed GitAgent Configuration

```yaml
plugins:
  lyzr-tools:
    enabled: true
    config:
      api_key: "${LYZR_API_KEY}"
      base_url: "https://agent-prod.studio.lyzr.ai"
      agent_id: "${GITAGENT_LYZR_AGENT_ID}"
      workspace_id: "${LYZR_WORKSPACE_ID}"
      prefer_lyzr_tools: true
```

### Swagger-Confirmed Lyzr API Contracts

The Swagger documentation for `https://agent-dev.test.studio.lyzr.ai/swagger#/` confirms that Lyzr already exposes tool discovery, credential, MCP, provider/action, and tool execution APIs. Therefore, the GitAgent implementation should use these existing `/v3` APIs instead of introducing the previously proposed `/v4/tools` endpoints.

Authentication in these endpoints is defined with `APIKeyHeader`, which uses the `x-api-key` header. The OpenAI-compatible chat endpoints use bearer auth separately.

#### General Tool Discovery

```http
GET /v3/tools/
x-api-key: <LYZR_API_KEY>
```

Swagger summary: `Get User Tools`

```http
GET /v3/tools/all/user
x-api-key: <LYZR_API_KEY>
```

Swagger summary: `Get All Tools`

The Swagger response schemas for these two list endpoints are generic objects, so the plugin should treat them as platform responses and normalize them internally.

#### Provider and Action Discovery

```http
GET /v3/providers/tools/actions/{provider_identifier}
x-api-key: <LYZR_API_KEY>
```

Swagger summary: `Get Tools Actions`

Query parameters:

- `tool_source`
- `app_id`

```http
GET /v3/providers/tools/all
x-api-key: <LYZR_API_KEY>
```

Swagger summary: `Get All Tools`

```http
GET /v3/providers/lyzr/aci-tools
x-api-key: <LYZR_API_KEY>
```

Swagger summary: `List Lyzr Aci Tools`

These endpoints are the best Swagger-confirmed candidates for discovering Lyzr/ACI-backed app tools such as Gmail and Slack, including action names that can later be placed into `ToolConfig.action_names`.

#### Connected Accounts and Credential Status

```http
GET /v3/tools/credentials/connected_accounts?user_id=<user_id>
x-api-key: <LYZR_API_KEY>
```

Swagger summary: `Get Tool Credential By User Id`

This endpoint should be used by the GitAgent bridge to determine which tool credentials are already connected for the user.

Credential creation and lifecycle endpoints are also present:

```http
POST /v3/tools/credentials/oauth
POST /v3/tools/credentials/static
PATCH /v3/tools/credentials/{credential_id}/status
GET /v3/tools/credentials/{credential_id}/test/supported
POST /v3/tools/credentials/{credential_id}/test
DELETE /v3/tools/credentials/{credential_id}
```

Relevant Swagger schemas:

```json
{
  "CreateOAuthToolCredentialModel": {
    "required": ["credential_name", "user_id", "provider_uuid"],
    "fields": {
      "credential_name": "string",
      "user_id": "string",
      "provider_uuid": "string",
      "redirect_url": "string | null",
      "grant_type": "authorization_code | client_credentials",
      "tenant_id": "string | null",
      "token_url": "string | null",
      "client_id": "string | null",
      "client_secret": "string | null",
      "scope": "string | null",
      "credentials": "object | null"
    }
  },
  "CreateStaticToolCredentialModel": {
    "required": ["credential_name", "user_id", "provider_uuid", "credentials"],
    "fields": {
      "credential_name": "string",
      "user_id": "string",
      "provider_uuid": "string",
      "credentials": "object"
    }
  }
}
```

#### MCP Server Tool Discovery and Execution

For tools exposed through MCP servers, Swagger confirms dedicated endpoints:

```http
GET /v3/tools/mcp/servers
x-api-key: <LYZR_API_KEY>
```

Swagger response schema: `MCPServerListResponse`

```http
GET /v3/tools/mcp/servers/{server_id}/tools
x-api-key: <LYZR_API_KEY>
```

Swagger response schema: `ToolsListResponse`

The relevant response schema is:

```json
{
  "server_id": "string",
  "server_name": "string",
  "tools": [
    {
      "name": "string",
      "display_name": "string | null",
      "description": "string | null",
      "input_schema": {}
    }
  ],
  "total": 0
}
```

MCP tool execution:

```http
POST /v3/tools/mcp/tools/execute
x-api-key: <LYZR_API_KEY>
Content-Type: application/json
```

Swagger request schema: `lyzr_agent__tools__mcp_tools__ToolExecuteRequest`

```json
{
  "server_id": "string",
  "tool_name": "string",
  "arguments": {}
}
```

Swagger response schema: `lyzr_agent__tools__mcp_tools__ToolExecuteResponse`

```json
{
  "server_id": "string",
  "tool_name": "string",
  "result": [],
  "success": true,
  "error": "string | null"
}
```

Swagger also confirms MCP OAuth flow support:

```http
POST /v3/tools/mcp/servers/{server_id}/oauth/initiate
GET /v3/tools/mcp/servers/{server_id}/oauth/status?state=<state>
```

#### Generic Inference Tool Execution

For agent-level tool execution outside the MCP-specific path, Swagger confirms:

```http
POST /v3/inference/tools/execute
x-api-key: <LYZR_API_KEY>
Content-Type: application/json
```

Swagger request schema: `api__factory__v3__inference__models__ToolExecuteRequest`

```json
{
  "agent_id": "string | null",
  "tool_name": "string",
  "tool_configs": [
    {
      "tool_name": "string",
      "tool_source": "string",
      "action_names": ["string"],
      "persist_auth": false,
      "server_id": "string | null",
      "provider_uuid": "string | null",
      "credential_id": "string | null"
    }
  ],
  "arguments": {},
  "trace_id": "string | null"
}
```

Swagger response schema: `api__factory__v3__inference__models__ToolExecuteResponse`

```json
{
  "tool_name": "string",
  "trace_id": "string",
  "result": {}
}
```

This is the strongest Swagger-confirmed candidate for a GitAgent Lyzr bridge that executes pre-authorized app tools, because `ToolConfig` includes `credential_id`, `provider_uuid`, `server_id`, `action_names`, and `persist_auth`.

#### OpenAI-Compatible Model Endpoint

The spec also confirms the model/chat path remains separate:

```http
POST /v4/chat/completions
Authorization: Bearer <LYZR_API_KEY>
```

This supports the RCA conclusion: model authentication and tool credential execution are separate API surfaces.

#### Remaining API Alignment Item

Swagger confirms the endpoints needed for discovery and execution, but the RCA still needs product/API confirmation for the exact response shape when a tool is unavailable or not authorized. In particular, the GitAgent bridge needs a deterministic way to map Lyzr responses into:

```json
{
  "status": "authorization_required",
  "provider": "gmail|slack|...",
  "auth_url": "https://..."
}
```

If Lyzr already returns this through connected-account or execution endpoints, the plugin should preserve that shape. If not, GitAgent should normalize current error payloads into this bridge-level result.

### Proposed Plugin Shape

The plugin can use the existing programmatic plugin API:

- `api.registerTool()` is available at `src/plugin-sdk.ts:18-19`.
- Programmatic tools are collected at `src/plugins.ts:237-284`.
- Those tools are merged into the active tool list in the CLI at `src/index.ts:545-560` and in the SDK at `src/sdk.ts:192-207`.

Pseudo-implementation:

```ts
export async function register(api) {
  const tools = await fetchLyzrTools(api.config);

  for (const tool of tools) {
    api.registerTool({
      name: normalizeToolName(tool.name),
      description: tool.description,
      inputSchema: tool.input_schema,
      handler: async (args) => {
        const result = await executeLyzrTool(api.config, {
          tool_name: tool.name,
          tool_source: tool.source,
          action_names: tool.action_names,
          credential_id: tool.credential_id,
          provider_uuid: tool.provider_uuid,
          server_id: tool.server_id
        }, args);
        if (result.status === "authorization_required") {
          return {
            text: `Authorization required for ${tool.display_name}: ${result.auth_url}`,
            details: result
          };
        }
        return {
          text: result.result?.text ?? JSON.stringify(result.result),
          details: result
        };
      }
    });
  }

  api.addPrompt(
    "Prefer Lyzr-backed tools for Gmail, Slack, and other connected apps when available. These tools use pre-authorized Lyzr ecosystem credentials."
  );
}
```

## Assurance Model

This implementation can provide assurance that pre-authorized tools are available only if Lyzr exposes authorized tool discovery and server-side execution.

Assurance condition:

```text
If a tool is authorized in Lyzr and included in Lyzr discovery,
then GitAgent will register it as an available tool.
```

Execution assurance:

```text
If GitAgent calls a registered Lyzr-backed tool,
then execution occurs through Lyzr using Lyzr-managed credentials,
not local Gmail/Slack credentials.
```

Non-assurance cases:

- Tool exists in GitAgent locally but is not discoverable from Lyzr.
- Tool is authorized in Lyzr but omitted from the discovery API response.
- Lyzr API key maps to a different workspace/user/agent than the one where the tool was authorized.
- Lyzr refuses to proxy execution and only exposes raw connector tokens, which should be avoided.

## Implementation Plan of Events

### Phase 0: Product and API Alignment

Owner: Lyzr platform + GitAgent integration team

Events:

1. Confirm which Swagger-confirmed path should be the primary execution path for GitAgent: generic `POST /v3/inference/tools/execute`, MCP `POST /v3/tools/mcp/tools/execute`, or both.
2. Confirm the discovery sequence for Gmail/Slack: connected accounts, provider/actions, all tools, MCP server tools, or a combined flow.
3. Define required identity scope: `user_id`, `agent_id`, `provider_uuid`, `credential_id`, `server_id`, workspace, organization, or project.
4. Define expected `LYZR_API_KEY` permissions for tool discovery, connected-account lookup, credential status, and execution.
5. Define auth-required and permission-denied error normalization if current Swagger responses do not already return a stable shape.
6. Decide naming convention for registered tools, for example `lyzr_gmail_send_email`.

Exit criteria:

- Swagger-backed endpoint sequence is documented for Gmail and Slack.
- Example Gmail and Slack discovery/execution payloads are available.
- Security confirms raw third-party OAuth tokens will not be returned to GitAgent.

### Phase 1: GitAgent Plugin Skeleton

Owner: GitAgent integration team

Events:

1. Create a `lyzr-tools` plugin directory with `plugin.yaml`.
2. Add config schema for `api_key`, `base_url`, `agent_id`, `workspace_id`, and `prefer_lyzr_tools`.
3. Add an entrypoint that uses `api.registerTool()` from the plugin API.
4. Add prompt text through `api.addPrompt()` to prefer Lyzr-backed tools.
5. Add basic unit tests for config resolution and plugin load failure modes.

Relevant existing integration points:

- Plugin config resolution: `src/plugins.ts:62-96`
- Plugin entrypoint loading: `src/plugins.ts:237-250`
- Plugin tool collection: `src/plugins.ts:251-268`
- Plugin API: `src/plugin-sdk.ts:10-36`

Exit criteria:

- Plugin loads through existing GitAgent plugin system.
- Plugin can register one static test tool.

### Phase 2: Tool Discovery Integration

Owner: GitAgent integration team + Lyzr API team

Events:

1. Implement `fetchLyzrTools(config)`.
2. Use Swagger-confirmed discovery inputs from `GET /v3/tools/`, `GET /v3/tools/all/user`, `GET /v3/providers/tools/actions/{provider_identifier}`, `GET /v3/providers/tools/all`, `GET /v3/providers/lyzr/aci-tools`, `GET /v3/tools/credentials/connected_accounts`, and MCP listing endpoints where applicable.
3. Normalize tool names to GitAgent-compatible identifiers.
4. Convert Lyzr `input_schema` / action schemas into GitAgent `inputSchema`.
5. Filter out unauthorized tools or register them with clear auth-required behavior depending on product decision.
6. Detect collisions with existing tool names.
7. Add telemetry/logging for discovered tools count and skipped tools.

Exit criteria:

- A user with authorized Gmail sees Gmail tool registered in GitAgent.
- A user without authorized Gmail sees a clear auth-required state, not a request for local SMTP credentials.

### Phase 3: Tool Execution Proxy

Owner: GitAgent integration team + Lyzr API team

Events:

1. Implement `executeLyzrTool(config, tool, args)` using `POST /v3/inference/tools/execute` for agent-level tools where possible.
2. Implement MCP execution fallback or parallel support using `POST /v3/tools/mcp/tools/execute` for MCP-backed tools.
3. Populate `ToolConfig` with `tool_name`, `tool_source`, `action_names`, and available `credential_id`, `provider_uuid`, or `server_id`.
4. Map execution success into GitAgent tool text result.
5. Map `authorization_required` or equivalent Lyzr errors into a user-facing result with provider and auth URL if available.
6. Map permission errors, validation errors, rate limits, and platform errors into structured `details`.
7. Ensure sensitive values are redacted from logs and tool results.
8. Add retry policy only for safe transient failures.

Exit criteria:

- Gmail send executes through Lyzr with no local `GMAIL_USER` or `GMAIL_APP_PASSWORD`.
- Slack send executes through Lyzr with no local Slack bot token.
- Tool result returns to the model as a normal GitAgent tool result.

### Phase 4: Local Duplicate Tool Deconfliction

Owner: GitAgent integration team

Events:

1. Add prompt guidance to prefer Lyzr tools when duplicate local skills exist.
2. Optionally add an allow/deny tool config that disables local duplicate skills/tools.
3. Consider auto-prefixing Lyzr tools with `lyzr_` to avoid name collisions.
4. Add documentation explaining how Lyzr-backed tools differ from local skills.

Relevant current behavior:

- CLI tool collision handling skips colliding plugin tools. See `src/index.ts:545-560`.
- SDK tool collision handling does the same. See `src/sdk.ts:192-207`.

Exit criteria:

- Model chooses `lyzr_gmail_send_email` rather than local `gmail-email` SMTP flow.
- Users are not instructed to create local app passwords when Lyzr Gmail is authorized.

### Phase 5: Tests and Validation

Owner: GitAgent integration team

Events:

1. Unit test discovery success with Gmail and Slack tools.
2. Unit test no tools returned.
3. Unit test `authorization_required`.
4. Unit test execution success.
5. Unit test execution failure and redaction.
6. Integration test against a mocked Lyzr API.
7. Manual E2E test with a real Lyzr account that has Gmail and Slack pre-authorized.

Acceptance scenarios:

```text
Given LYZR_API_KEY belongs to a user with Gmail authorized
When GitAgent starts with lyzr-tools enabled
Then GitAgent registers a Gmail send tool
And sending email does not ask for GMAIL_USER or GMAIL_APP_PASSWORD
And execution is proxied through Lyzr
```

```text
Given LYZR_API_KEY belongs to a user without Slack authorized
When GitAgent attempts to use Slack
Then GitAgent returns authorization_required with a Lyzr auth URL
And does not ask for local Slack bot credentials
```

### Phase 6: Rollout

Owner: Product + engineering

Events:

1. Release plugin behind a feature flag.
2. Enable for internal dogfood accounts.
3. Track metrics: discovery success, execution success, auth-required rate, tool errors.
4. Add docs to install/setup flow.
5. Deprecate local Gmail/Slack credential instructions for Lyzr mode.
6. Roll out broadly after successful internal validation.

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Multiple Swagger-confirmed discovery paths exist | Plugin may choose incomplete source of truth | Define canonical discovery sequence for Gmail/Slack before implementation |
| Generic and MCP execution paths differ | Tool execution behavior may be inconsistent | Route tools by source: generic `/v3/inference/tools/execute` for agent tools, MCP `/v3/tools/mcp/tools/execute` for MCP tools |
| Tool names collide with local tools | Wrong tool may be selected | Prefix Lyzr tools and add prompt preference |
| API key maps to wrong user/workspace/org | Tools appear missing | Require explicit `user_id` and any required org/workspace context in plugin config |
| Auth-required errors are vague or inconsistent | User confusion persists | Normalize Lyzr errors into structured `auth_url`, provider, and reason |
| Sensitive args/results leak in logs | Security issue | Redact secrets and PII in plugin logging |

## Final Recommendation

Proceed with a Lyzr-backed tool bridge rather than trying to pass Gmail/Slack credentials into GitAgent.

The implementation should guarantee this behavior:

```text
Lyzr-authorized tool
  -> discovered by GitAgent
  -> registered as a GitAgent proxy tool
  -> executed by Lyzr server-side
  -> no local reauthorization required
```

This design aligns with the current GitAgent plugin architecture, avoids local credential duplication, and preserves Lyzr as the system of record for connected app authorization.
