# lyzr-tools

A gitagent plugin that discovers tools already authorized in [Lyzr](https://lyzr.ai) — Gmail, Slack, and other connected apps — and registers them as gitagent tools that execute **through Lyzr's server-side, pre-authorized credential vault**. Without this plugin, `LYZR_API_KEY` is only wired into gitagent's model path; tool calls fall back to local skills that need their own credentials (e.g. `GMAIL_USER`/`GMAIL_APP_PASSWORD`).

See [`docs/lyzr-tool-auth-rca.md`](../../docs/lyzr-tool-auth-rca.md) for the full root-cause analysis and design, and [`docs/lyzr-tool-bridge-test-cases.md`](../../docs/lyzr-tool-bridge-test-cases.md) for the acceptance criteria this plugin targets.

## What it does

1. On load, reads `LYZR_API_KEY` (and related config) and calls Lyzr's `/v3` tool APIs to discover:
   - Provider/app actions for each configured provider (default: `gmail`, `slack`) via `GET /v3/providers/tools/actions/{provider}`.
   - Tools exposed through Lyzr MCP servers via `GET /v3/tools/mcp/servers` + `.../{server_id}/tools`.
   - Which of those are already authorized for the configured user via `GET /v3/tools/credentials/connected_accounts`.
2. Registers each discovered tool as a gitagent tool named `lyzr_<provider>_<action>` (or `lyzr_mcp_<server>_<tool>` for MCP tools).
3. Executes tool calls by proxying to `POST /v3/inference/tools/execute` (provider/action tools) or `POST /v3/tools/mcp/tools/execute` (MCP tools).
4. If a tool isn't authorized, calling it returns a structured `authorization_required` result instead of asking for local credentials.
5. Adds prompt guidance telling the model to prefer `lyzr_*` tools over local duplicate skills (e.g. the bundled `gmail-email` skill).

## Setup

The plugin is enabled by default in this repo's `agent.yaml`. It no-ops (with a single warning log line) if `LYZR_API_KEY` isn't set — it will not attempt any network calls without a key.

```bash
export LYZR_API_KEY="<your-lyzr-api-key>"
# Optional, defaults shown:
export LYZR_BASE_URL="https://agent-prod.studio.lyzr.ai"
export LYZR_USER_ID="<lyzr-user-id>"          # needed to resolve authorization status
export GITAGENT_LYZR_AGENT_ID="<lyzr-agent-id>" # needed for agent-level tool execution
export LYZR_TOOL_PROVIDERS="gmail,slack"       # comma-separated provider identifiers to discover
```

Or configure it explicitly in `agent.yaml`:

```yaml
plugins:
  lyzr-tools:
    enabled: true
    config:
      api_key: "${LYZR_API_KEY}"
      base_url: "https://agent-prod.studio.lyzr.ai"
      agent_id: "${GITAGENT_LYZR_AGENT_ID}"
      user_id: "${LYZR_USER_ID}"
      providers: "gmail,slack"
      prefer_lyzr_tools: true
```

## Known limitations / open items

- `GET /v3/tools/` and `GET /v3/tools/all/user` are not used as discovery sources: their Swagger response schema is a generic `{}` object with no documented shape to normalize. The client (`lib/client.ts`) still exposes them for future use once Lyzr documents a concrete response shape.
- The exact field pairing for `POST /v3/inference/tools/execute` (which value goes in the top-level `tool_name` vs. `ToolConfig.tool_name`) isn't fully pinned by the Swagger schema. `lib/execute.ts` documents the assumption made; this is flagged in the RCA as a "Remaining API Alignment Item" that needs product/API confirmation.
- Authorization-required detection uses HTTP status codes plus a keyword heuristic over the error body (`lib/execute.ts: detectAuthRequired`), since Lyzr doesn't yet document a stable `authorization_required` response shape for these endpoints. If/when Lyzr standardizes that shape, replace the heuristic with a direct field check.

## Testing

Unit tests live in [`test/lyzr-tools.test.ts`](../../test/lyzr-tools.test.ts) at the repo root (consistent with gitagent's existing `test/*.test.ts` convention) and run via:

```bash
npm test
```

They exercise discovery (success, empty, provider errors), execution (success, error, authorization-required, MCP), redaction, and name normalization — all against a fake `LyzrClient`, with no real network calls.
