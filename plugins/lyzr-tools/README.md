# lyzr-tools

A gitagent plugin that discovers tools already authorized in [Lyzr](https://lyzr.ai) — Gmail, Slack, and other connected apps — and registers them as gitagent tools that execute **through Lyzr's server-side, pre-authorized credential vault**. Without this plugin, `LYZR_API_KEY` is only wired into gitagent's model path; tool calls fall back to local skills that need their own credentials (e.g. `GMAIL_USER`/`GMAIL_APP_PASSWORD`).

See [`docs/lyzr-tool-auth-rca.md`](../../docs/lyzr-tool-auth-rca.md) for the full root-cause analysis and design, and [`docs/lyzr-tool-bridge-test-cases.md`](../../docs/lyzr-tool-bridge-test-cases.md) for the acceptance criteria this plugin targets.

## What it does

1. On load, reads `LYZR_API_KEY`/`GITAGENT_LYZR_AGENT_ID` (and related config) and calls Lyzr's `/v3` tool APIs to discover:
   - The configured agent's own `tool_configs` via `GET /v3/agents/{agent_id}` — each entry is a connected integration a human already wired up in Lyzr Studio (tool_name, tool_source, action_names, provider_uuid, credential_id), used verbatim rather than reconstructed.
   - Tools exposed through Lyzr MCP servers via `GET /v3/tools/mcp/servers` + `.../{server_id}/tools`.
   - Connected-account status via `GET /v3/tools/credentials/connected_accounts`, as a secondary authorization signal alongside each tool_config's own `credential_id`.
2. Registers one gitagent tool per `action_names` entry, named after the action (e.g. `lyzr_gmail_send_email` for `GMAIL_SEND_EMAIL`), or `lyzr_mcp_<server>_<tool>` for MCP tools.
3. Executes tool calls by proxying to `POST /v3/inference/tools/execute` (provider/action tools) or `POST /v3/tools/mcp/tools/execute` (MCP tools).
4. If a tool isn't authorized, calling it returns a structured `authorization_required` result instead of asking for local credentials.
5. Adds prompt guidance telling the model to prefer `lyzr_*` tools over local duplicate skills (e.g. the bundled `gmail-email` skill).

## Setup

The plugin is enabled by default in this repo's `agent.yaml`. It no-ops (with a single warning log line) if `LYZR_API_KEY` isn't set — it will not attempt any network calls without a key.

```bash
export LYZR_API_KEY="<your-lyzr-api-key>"
export GITAGENT_LYZR_AGENT_ID="<lyzr-agent-id>" # required: source of tool_configs to discover, and target for execution
# Optional, defaults shown:
export LYZR_BASE_URL="https://agent-prod.studio.lyzr.ai"
export LYZR_USER_ID="<lyzr-user-id>"          # secondary signal for resolving authorization status
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
      prefer_lyzr_tools: true
```

## Known limitations / open items

- **No per-action input schema.** `GET /v3/agents/{agent_id}` (the discovery source, confirmed against a live account) exposes each connected integration's `tool_name`/`tool_source`/`action_names`/`provider_uuid`/`credential_id`, but nothing documenting an action's parameters (e.g. `GMAIL_SEND_EMAIL`'s `to`/`subject`/`body`). Registered tools currently get a permissive empty `inputSchema`, so the model must infer arguments from the tool's name/description alone. A targeted fast-follow would fetch schemas per matched action from `GET /v3/providers/tools/actions/{provider_id}?tool_source=...` (still exposed on `lib/client.ts`) without reintroducing catalog-based tool_config reconstruction.
- `GET /v3/tools/` and `GET /v3/tools/all/user` are not used as discovery sources: their Swagger response schema is a generic `{}` object with no documented shape to normalize. The client (`lib/client.ts`) still exposes them for future use once Lyzr documents a concrete response shape.
- `GET /v3/providers/tools/all` (the provider catalog) is no longer used for discovery — an earlier version of this plugin reconstructed `tool_configs` entries from it, but a real agent's own `tool_configs[].tool_name` turned out to be a human-named connected-integration label (e.g. `"gmail-Akshat Gmail Integration"`), not the catalog's generic `provider_id`, and `provider_uuid` didn't match `meta_data.app_id` either. Reading the agent's own config directly avoids that whole class of guesswork. The catalog client method remains available for the input-schema fast-follow above.
- The exact field pairing for `POST /v3/inference/tools/execute` (specifically: whether the top-level `tool_name` is "the action to invoke" while `tool_configs[0]` is "the credential context it runs under") isn't fully pinned by the Swagger schema — it's inferred from the shape of a real agent's stored config, not from a captured real execute request/response. Flagged in the RCA as a "Remaining API Alignment Item."
- Authorization detection treats a non-empty `credential_id` already present on the agent's tool_config as primary evidence of authorization, OR'd with `GET /v3/tools/credentials/connected_accounts` status. At execution time, HTTP status codes plus a keyword heuristic over the error body (`lib/execute.ts: detectAuthRequired`) still catch anything that slips through, since Lyzr doesn't yet document a stable `authorization_required` response shape.
- **Redaction is key-name-based first, shape-based second.** `lib/redact.ts` masks any field whose *key* looks sensitive (`token`, `secret`, `credential`, etc.), and additionally checks string leaves by *shape* (known token prefixes, JWTs, long opaque alphanumeric strings) so a raw OAuth token returned under an innocuous key like `result` still gets masked. This is a heuristic, not a guarantee: Lyzr's execution backend should not return raw credential blobs in tool `result` payloads in the first place — if it does and the value doesn't match the shape heuristic (e.g. a short-lived token or an unusual format), it can still reach model context.

## Testing

Unit tests live in [`test/lyzr-tools.test.ts`](../../test/lyzr-tools.test.ts) at the repo root (consistent with gitagent's existing `test/*.test.ts` convention) and run via:

```bash
npm test
```

They exercise discovery (success, empty, provider errors), execution (success, error, authorization-required, MCP), redaction, and name normalization — all against a fake `LyzrClient`, with no real network calls.
