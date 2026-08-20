# Test Cases: Lyzr Pre-Authorized Tool Bridge for GitAgent

Date: 2026-07-15
Scope: Reproduce the current duplicate-authorization issue and validate the proposed GitAgent integration with Lyzr's Swagger-confirmed tool APIs.

## Preconditions

- GitAgent repo is available locally.
- A Lyzr dev/staging account exists with a valid `LYZR_API_KEY`.
- At least one Lyzr agent exists, with `GITAGENT_LYZR_AGENT_ID` available.
- Test user A has Gmail and Slack authorized inside Lyzr.
- Test user B does not have Gmail or Slack authorized inside Lyzr.
- Lyzr Swagger APIs are reachable:
  - `GET /v3/tools/`
  - `GET /v3/tools/all/user`
  - `GET /v3/providers/tools/actions/{provider_identifier}`
  - `GET /v3/providers/tools/all`
  - `GET /v3/tools/credentials/connected_accounts`
  - `POST /v3/inference/tools/execute`
  - MCP APIs under `/v3/tools/mcp/*`

## Part A: Reproduce Current Issue

### TC-A01: Lyzr API Key Enables Model but Not Local Gmail Tool

Objective: Prove that `LYZR_API_KEY` currently works for the model path but not for Gmail tool authorization.

Steps:

1. Set only Lyzr model credentials:
   ```bash
   export LYZR_API_KEY="<valid-key>"
   export GITAGENT_LYZR_AGENT_ID="<agent-id>"
   unset GMAIL_USER
   unset GMAIL_APP_PASSWORD
   ```
2. Run GitAgent with the Lyzr model backend.
3. Ask: "Send an email to qa@example.com with subject Test and body Hello."
4. If GitAgent chooses the bundled Gmail skill, observe the result.

Expected current behavior:

- Model call succeeds through Lyzr.
- Gmail action fails or asks for local `GMAIL_USER` and `GMAIL_APP_PASSWORD`.
- User is effectively asked to authorize/configure Gmail again, despite Gmail possibly being authorized in Lyzr.

Pass condition:

- The issue is reproduced when local Gmail credentials are required.

### TC-A02: Bundled Gmail Skill Uses SMTP Credentials

Objective: Confirm current Gmail path is independent of Lyzr.

Steps:

1. Ensure `LYZR_API_KEY` is set.
2. Ensure `GMAIL_USER` and `GMAIL_APP_PASSWORD` are unset.
3. Run:
   ```bash
   python3 skills/gmail-email/scripts/send_email.py \
     --to "qa@example.com" \
     --subject "Test" \
     --body "Hello"
   ```

Expected current behavior:

- Script prints `ERROR: Gmail credentials not found!`
- Script asks for `GMAIL_USER` and `GMAIL_APP_PASSWORD`.

Pass condition:

- The script does not use `LYZR_API_KEY`.

### TC-A03: Slack or Other Local Tool Requires Independent Credential

Objective: Confirm the same class of issue exists for non-Gmail tools if implemented locally.

Steps:

1. Configure Lyzr credentials only.
2. Trigger a Slack action through any local Slack skill/tool if present.
3. Do not provide local Slack bot/user tokens.

Expected current behavior:

- Local Slack tool requires its own Slack credentials.
- Lyzr pre-authorization is not reused.

Pass condition:

- The issue is reproduced for at least one non-Gmail connected app, or marked not applicable if no local Slack tool exists.

## Part B: Validate Lyzr Swagger API Availability

### TC-B01: List User Tools

Objective: Confirm `GET /v3/tools/` is reachable with `x-api-key`.

Steps:

1. Call:
   ```bash
   curl -sS \
     -H "x-api-key: $LYZR_API_KEY" \
     "https://agent-dev.test.studio.lyzr.ai/v3/tools/"
   ```
2. Inspect response.

Expected behavior:

- API returns 200.
- Response contains user tool data or an empty user tool collection.

Pass condition:

- Response is authenticated and parseable.

### TC-B02: List All User Tools

Objective: Confirm `GET /v3/tools/all/user` returns available tools.

Steps:

1. Call:
   ```bash
   curl -sS \
     -H "x-api-key: $LYZR_API_KEY" \
     "https://agent-dev.test.studio.lyzr.ai/v3/tools/all/user"
   ```

Expected behavior:

- API returns 200.
- Response includes available tool/provider data, or a valid empty response.

Pass condition:

- GitAgent bridge can use or normalize the response.

### TC-B03: List Connected Accounts for Authorized User

Objective: Confirm Lyzr can report connected tool credentials for a user.

Steps:

1. Use test user A who has Gmail and Slack authorized.
2. Call:
   ```bash
   curl -sS \
     -H "x-api-key: $LYZR_API_KEY" \
     "https://agent-dev.test.studio.lyzr.ai/v3/tools/credentials/connected_accounts?user_id=<user-a-id>"
   ```

Expected behavior:

- API returns 200.
- Response indicates connected Gmail and Slack accounts, or includes credential identifiers usable by execution.

Pass condition:

- Response includes enough metadata to map a connected account to `credential_id`, provider, or tool configuration.

### TC-B04: List Connected Accounts for Unauthorized User

Objective: Confirm unauthorized state can be detected.

Steps:

1. Use test user B who has no Gmail/Slack authorization.
2. Call connected accounts endpoint with user B.

Expected behavior:

- API returns 200.
- Response does not include Gmail/Slack credentials.

Pass condition:

- Bridge can detect "not authorized" without asking for local credentials.

### TC-B05: Provider Action Discovery

Objective: Confirm provider/action endpoint can list app actions.

Steps:

1. Call:
   ```bash
   curl -sS \
     -H "x-api-key: $LYZR_API_KEY" \
     "https://agent-dev.test.studio.lyzr.ai/v3/providers/tools/actions/<provider_identifier>?tool_source=<source>&app_id=<app-id>"
   ```
2. Use actual provider identifier/source/app ID from Lyzr configuration.

Expected behavior:

- API returns action names for the provider/app.
- Gmail action such as send email or Slack action such as send message is discoverable if configured.

Pass condition:

- Actions can be transformed into GitAgent tool definitions.

### TC-B06: Generic Tool Execution API Contract

Objective: Confirm `POST /v3/inference/tools/execute` accepts `ToolConfig`.

Steps:

1. Prepare a payload using a known authorized Gmail or Slack action:
   ```json
   {
     "agent_id": "<agent-id>",
     "tool_name": "<tool-name>",
     "tool_configs": [
       {
         "tool_name": "<tool-name>",
         "tool_source": "<tool-source>",
         "action_names": ["<action-name>"],
         "persist_auth": true,
         "provider_uuid": "<provider-uuid>",
         "credential_id": "<credential-id>"
       }
     ],
     "arguments": {},
     "trace_id": "qa-test"
   }
   ```
2. Call:
   ```bash
   curl -sS \
     -X POST \
     -H "x-api-key: $LYZR_API_KEY" \
     -H "Content-Type: application/json" \
     -d @payload.json \
     "https://agent-dev.test.studio.lyzr.ai/v3/inference/tools/execute"
   ```

Expected behavior:

- API returns 200 for valid authorized tool calls.
- Response contains `tool_name`, `trace_id`, and `result`.

Pass condition:

- The response can be mapped into a GitAgent tool result.

## Part C: Validate GitAgent Lyzr Tool Bridge Implementation

These tests apply after the `lyzr-tools` GitAgent plugin/provider is implemented.

### TC-C01: Plugin Loads Successfully

Objective: Confirm GitAgent loads the Lyzr bridge plugin.

Steps:

1. Configure `agent.yaml`:
   ```yaml
   plugins:
     lyzr-tools:
       enabled: true
       config:
         api_key: "${LYZR_API_KEY}"
         base_url: "https://agent-dev.test.studio.lyzr.ai"
         agent_id: "${GITAGENT_LYZR_AGENT_ID}"
         user_id: "<user-a-id>"
   ```
2. Start GitAgent.

Expected behavior:

- GitAgent logs or exposes that `lyzr-tools` plugin loaded.
- No plugin config warnings for required fields.

Pass condition:

- Plugin is present in `/plugins` output or startup logs.

### TC-C02: Authorized Gmail Tool Is Registered

Objective: Confirm GitAgent registers Lyzr-backed Gmail tool for user A.

Steps:

1. Use user A with Gmail authorized in Lyzr.
2. Start GitAgent with `lyzr-tools`.
3. Inspect active tools through startup output or SDK messages.

Expected behavior:

- A Gmail send tool appears, for example `lyzr_gmail_send_email`.
- Tool description says it uses Lyzr-backed/pre-authorized credentials.

Pass condition:

- Tool is registered without local Gmail credentials.

### TC-C03: Authorized Slack Tool Is Registered

Objective: Confirm GitAgent registers Lyzr-backed Slack tool for user A.

Steps:

1. Use user A with Slack authorized in Lyzr.
2. Start GitAgent with `lyzr-tools`.
3. Inspect active tools.

Expected behavior:

- Slack action tool appears, for example `lyzr_slack_send_message`.

Pass condition:

- Tool is registered without local Slack credentials.

### TC-C04: Unauthorized Tool Produces Auth-Required State

Objective: Confirm user B does not get local credential prompts.

Steps:

1. Use user B without Gmail authorization.
2. Start GitAgent with `lyzr-tools`.
3. Ask: "Send an email to qa@example.com."

Expected behavior:

- GitAgent does not ask for `GMAIL_USER` or `GMAIL_APP_PASSWORD`.
- GitAgent returns a structured auth-required result or message.
- If available from Lyzr, the result includes provider and auth URL.

Pass condition:

- Missing authorization is represented as Lyzr auth-required, not local credential setup.

### TC-C05: Gmail Send Executes Through Lyzr

Objective: Validate full happy path for Gmail.

Steps:

1. Use user A with Gmail authorized.
2. Ensure local Gmail credentials are unset:
   ```bash
   unset GMAIL_USER
   unset GMAIL_APP_PASSWORD
   ```
3. Ask GitAgent: "Send an email to qa@example.com with subject Bridge Test and body This came through Lyzr."
4. Observe tool call and result.
5. Check recipient inbox or Lyzr execution logs.

Expected behavior:

- GitAgent calls Lyzr-backed Gmail tool.
- Lyzr executes the email send.
- Email is delivered or execution result confirms success.
- No local Gmail credentials are required.

Pass condition:

- Email send succeeds through Lyzr.

### TC-C06: Slack Send Executes Through Lyzr

Objective: Validate full happy path for Slack.

Steps:

1. Use user A with Slack authorized.
2. Ensure local Slack tokens are unset.
3. Ask GitAgent: "Send a Slack message to #qa saying Bridge test passed."
4. Observe tool call and result.
5. Check Slack channel or Lyzr execution logs.

Expected behavior:

- GitAgent calls Lyzr-backed Slack tool.
- Slack message is sent.
- No local Slack credentials are required.

Pass condition:

- Slack send succeeds through Lyzr.

### TC-C07: Tool Result Mapping

Objective: Ensure Lyzr execution results are returned cleanly to the model.

Steps:

1. Execute a Lyzr-backed tool through GitAgent.
2. Capture GitAgent `tool_result` event or CLI output.

Expected behavior:

- Result is human-readable.
- Raw implementation details are stored in `details` where available.
- Sensitive credentials/tokens are not printed.

Pass condition:

- Tool result can be safely shown to user and fed back to model.

### TC-C08: Local Duplicate Tool Is Not Preferred

Objective: Ensure GitAgent prefers Lyzr-backed tools over local duplicate skills.

Steps:

1. Ensure bundled `gmail-email` skill exists.
2. Enable Lyzr Gmail bridge tool.
3. Ask: "Send an email to qa@example.com."

Expected behavior:

- Model selects `lyzr_gmail_send_email`, not local SMTP skill.
- No local Gmail App Password prompt appears.

Pass condition:

- Lyzr-backed tool wins over local duplicate.

### TC-C09: Invalid API Key Fails Clearly

Objective: Validate failure behavior for bad `LYZR_API_KEY`.

Steps:

1. Set invalid key:
   ```bash
   export LYZR_API_KEY="invalid"
   ```
2. Start GitAgent with `lyzr-tools`.

Expected behavior:

- Plugin fails discovery gracefully.
- User sees clear authentication error.
- GitAgent itself does not crash unless configured to fail closed.

Pass condition:

- Error is actionable and does not expose secrets.

### TC-C10: Wrong User or Workspace Context

Objective: Validate behavior when API key is valid but user/workspace context does not match authorization.

Steps:

1. Use valid `LYZR_API_KEY`.
2. Configure wrong `user_id` or workspace context.
3. Start GitAgent and request Gmail/Slack action.

Expected behavior:

- Tool is not registered or returns auth-required/permission-denied.
- Error explains context mismatch or missing connected account.

Pass condition:

- No local credential prompt appears.
- No raw OAuth tokens are exposed.

### TC-C11: MCP Tool Discovery

Objective: Validate MCP-backed tools if Gmail/Slack are exposed through MCP.

Steps:

1. Ensure an MCP server exists in Lyzr.
2. Call bridge discovery.
3. Confirm bridge calls:
   - `GET /v3/tools/mcp/servers`
   - `GET /v3/tools/mcp/servers/{server_id}/tools`

Expected behavior:

- MCP server tools are converted into GitAgent tools.
- Tool schema uses `ToolResponse.input_schema`.

Pass condition:

- MCP-backed tool is registered and callable.

### TC-C12: MCP OAuth Flow

Objective: Validate OAuth flow handoff if MCP server requires authorization.

Steps:

1. Use an MCP server requiring OAuth.
2. Start bridge discovery.
3. Trigger OAuth initiation if status is unauthenticated.

Expected behavior:

- Bridge calls `POST /v3/tools/mcp/servers/{server_id}/oauth/initiate`.
- User receives auth URL or equivalent next step.
- Bridge can poll/check `GET /v3/tools/mcp/servers/{server_id}/oauth/status?state=<state>`.

Pass condition:

- User can authorize via Lyzr, and GitAgent does not request local credentials.

## Part D: Regression Tests

### TC-D01: Non-Lyzr Model Still Works

Objective: Ensure bridge does not break OpenAI/Anthropic model use.

Steps:

1. Configure GitAgent with a non-Lyzr model.
2. Disable or omit `lyzr-tools`.
3. Run a normal prompt.

Expected behavior:

- GitAgent works as before.

Pass condition:

- No regression in non-Lyzr flows.

### TC-D02: Lyzr Model Without Tool Bridge Still Works

Objective: Ensure existing Lyzr model flow remains functional without tool bridge.

Steps:

1. Configure Lyzr model backend.
2. Do not enable `lyzr-tools`.
3. Ask a normal non-tool prompt.

Expected behavior:

- Model call works.
- No tool discovery is attempted.

Pass condition:

- Existing Lyzr chat behavior is preserved.

### TC-D03: Tool Bridge Does Not Leak Credentials

Objective: Verify secrets are redacted.

Steps:

1. Execute Gmail/Slack through bridge.
2. Inspect CLI logs, SDK events, telemetry, and Lyzr returned result.

Expected behavior:

- No OAuth access token, refresh token, client secret, Slack bot token, Gmail app password, or raw credential blob is printed.

Pass condition:

- Logs contain only safe IDs and execution status.

### TC-D04: Tool Collision Handling

Objective: Validate duplicate tool names are handled.

Steps:

1. Create a local tool with same name as a Lyzr bridge tool.
2. Start GitAgent.

Expected behavior:

- Collision is detected.
- Lyzr tool is prefixed or local duplicate is skipped according to product decision.

Pass condition:

- Startup does not silently select the wrong tool.

## Acceptance Criteria Summary

Implementation is considered successful when:

- `LYZR_API_KEY` authenticates GitAgent to Lyzr tool APIs via `x-api-key`.
- GitAgent discovers Lyzr-authorized Gmail/Slack tools.
- GitAgent registers discovered tools as callable agent tools.
- GitAgent executes Gmail/Slack through Lyzr, not local credentials.
- Users with pre-authorized tools are not asked to authorize locally.
- Users without authorization receive a structured Lyzr auth-required response.
- No raw third-party OAuth tokens or app passwords are exposed to GitAgent users/logs.
- Existing non-Lyzr and Lyzr-model-only flows continue to work.
