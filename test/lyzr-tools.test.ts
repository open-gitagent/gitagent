// Unit tests for the lyzr-tools plugin (plugins/lyzr-tools).
//
// Strategy: exercise the plugin's library modules directly against a fake
// LyzrClient — no real network calls, no dependency on a live Lyzr account.
// This covers docs/lyzr-tool-bridge-test-cases.md Part C/D scenarios that
// don't require an actual GitAgent process or Lyzr backend.

import test, { describe, it } from "node:test";
import assert from "node:assert/strict";

import type { LyzrClient, LyzrResult } from "../plugins/lyzr-tools/lib/client.ts";
import { resolveConfig } from "../plugins/lyzr-tools/lib/config.ts";
import { buildDedupePrompt } from "../plugins/lyzr-tools/lib/dedupe.ts";
import { discoverLyzrTools } from "../plugins/lyzr-tools/lib/discover.ts";
import { detectAuthRequired, executeLyzrTool } from "../plugins/lyzr-tools/lib/execute.ts";
import { normalizeProviderKey, normalizeToolName } from "../plugins/lyzr-tools/lib/normalize.ts";
import { redactSecrets } from "../plugins/lyzr-tools/lib/redact.ts";
import type { LyzrDiscoveredTool, Logger, ResolvedConfig } from "../plugins/lyzr-tools/lib/types.ts";
import { registerWithClient } from "../plugins/lyzr-tools/index.ts";
import type { GitagentPluginApi } from "../src/plugin-sdk.ts";

// ── Test scaffolding ───────────────────────────────────────────────────

function ok<T>(data: T): LyzrResult<T> {
	return { ok: true, status: 200, data };
}

function fail(status: number, data?: unknown, error?: string): LyzrResult {
	return { ok: false, status, data, error: error ?? `HTTP ${status}` };
}

function silentLogger(): Logger & { messages: string[] } {
	const messages: string[] = [];
	return {
		messages,
		info: (m) => messages.push(`info: ${m}`),
		warn: (m) => messages.push(`warn: ${m}`),
		error: (m) => messages.push(`error: ${m}`),
	};
}

function fakeClient(overrides: Partial<LyzrClient> = {}): LyzrClient & { calls: Array<{ method: string; args: any[] }> } {
	const calls: Array<{ method: string; args: any[] }> = [];
	const record =
		<A extends any[], R>(method: string, fn: (...args: A) => Promise<R>) =>
		async (...args: A) => {
			calls.push({ method, args });
			return fn(...args);
		};

	const defaults: LyzrClient = {
		getAgent: async () => ok({ tool_configs: [] }),
		listUserTools: async () => ok([]),
		listAllUserTools: async () => ok([]),
		listConnectedAccounts: async () => ok([]),
		listProviderActions: async () => ok([]),
		listAllProviderTools: async () => ok([]),
		listAciTools: async () => ok([]),
		listMcpServers: async () => ok([]),
		listMcpServerTools: async () => ok({ tools: [] }),
		executeInferenceTool: async () => ok({ result: "ok", trace_id: "t-1" }),
		executeMcpTool: async () => ok({ success: true, result: ["ok"] }),
		initiateMcpOAuth: async () => ok({ auth_url: "https://lyzr.example/oauth" }),
		getMcpOAuthStatus: async () => ok({ status: "pending" }),
	};

	const merged = { ...defaults, ...overrides };
	const wrapped: any = {};
	for (const key of Object.keys(merged) as (keyof LyzrClient)[]) {
		wrapped[key] = record(key, merged[key] as any);
	}
	return { ...wrapped, calls };
}

function baseConfig(overrides: Partial<ResolvedConfig> = {}): ResolvedConfig {
	return {
		apiKey: "test-key",
		baseUrl: "https://agent-prod.studio.lyzr.ai",
		agentId: "agent-1",
		userId: "user-1",
		includeMcp: true,
		preferLyzrTools: true,
		timeoutMs: 5000,
		...overrides,
	};
}

// Mirrors the real tool_configs shape observed on a live Lyzr agent
// (GET /v3/agents/{agent_id}): a human-named connected-integration label as
// tool_name, UPPERCASE_SNAKE_CASE action_names, and a pre-resolved
// provider_uuid/credential_id.
function gmailToolConfig(overrides: Record<string, unknown> = {}) {
	return {
		tool_name: "gmail-Akshat Gmail Integration",
		tool_source: "composio",
		action_names: ["GMAIL_SEND_EMAIL"],
		persist_auth: true,
		server_id: null,
		provider_uuid: "6980d819100ddff45dec4e80",
		credential_id: "82be4f7e-cred-1",
		...overrides,
	};
}

// ── normalize.ts ───────────────────────────────────────────────────────

describe("normalizeToolName", () => {
	it("prefixes with lyzr_", () => {
		assert.equal(normalizeToolName("gmail_send_email"), "lyzr_gmail_send_email");
	});

	it("does not double-prefix an already-prefixed name", () => {
		assert.equal(normalizeToolName("lyzr_gmail_send_email"), "lyzr_gmail_send_email");
	});

	it("strips special characters and collapses whitespace/dashes", () => {
		assert.equal(normalizeToolName("Gmail: Send Email! (v2)"), "lyzr_gmail_send_email_v2");
	});

	it("falls back to a safe default for empty input", () => {
		assert.equal(normalizeToolName(""), "lyzr_tool");
	});
});

describe("normalizeProviderKey", () => {
	it("lowercases and replaces non-alphanumeric runs with underscores", () => {
		assert.equal(normalizeProviderKey("Google Workspace MCP"), "google_workspace_mcp");
	});
});

// ── redact.ts ──────────────────────────────────────────────────────────

describe("redactSecrets", () => {
	it("masks values under sensitive keys", () => {
		const redacted: any = redactSecrets({
			credential_id: "cred_abcdef123456",
			access_token: "shpat_1234567890abcdef",
			nested: { client_secret: "s3cr3tvalue" },
		});
		assert.notEqual(redacted.credential_id, "cred_abcdef123456");
		assert.ok(!String(redacted.credential_id).includes("abcdef123456"));
		assert.notEqual(redacted.access_token, "shpat_1234567890abcdef");
		assert.notEqual(redacted.nested.client_secret, "s3cr3tvalue");
	});

	it("leaves non-sensitive fields (including long text) untouched", () => {
		const body = "This came through Lyzr. ".repeat(5);
		const redacted: any = redactSecrets({ status: "success", result: body, tool: "lyzr_gmail_send_email" });
		assert.equal(redacted.result, body);
		assert.equal(redacted.status, "success");
		assert.equal(redacted.tool, "lyzr_gmail_send_email");
	});

	it("handles arrays and nested structures", () => {
		const redacted: any = redactSecrets({ accounts: [{ token: "abc123def456" }, { token: "xyz789uvw012" }] });
		assert.notEqual(redacted.accounts[0].token, "abc123def456");
		assert.notEqual(redacted.accounts[1].token, "xyz789uvw012");
	});
});

// ── discover.ts ────────────────────────────────────────────────────────

describe("discoverLyzrTools", () => {
	it("discovers one tool per action from the agent's own tool_configs, verbatim", async () => {
		const client = fakeClient({
			getAgent: async (agentId: string) => {
				assert.equal(agentId, "agent-1");
				return ok({
					tool_configs: [
						gmailToolConfig(),
						{
							tool_name: "composio_search",
							tool_source: "composio",
							action_names: ["COMPOSIO_SEARCH_SEARCH"],
							persist_auth: true,
							provider_uuid: "puid-search",
							credential_id: "",
						},
					],
				});
			},
			listConnectedAccounts: async () =>
				ok([{ provider: "composio_search", status: "connected", credential_id: "cred-search-1" }]),
		});

		const { tools, stats } = await discoverLyzrTools(client, baseConfig(), silentLogger());

		assert.equal(tools.length, 2);
		const gmail = tools.find((t) => t.actionName === "GMAIL_SEND_EMAIL")!;
		const search = tools.find((t) => t.actionName === "COMPOSIO_SEARCH_SEARCH")!;

		assert.equal(gmail.toolName, "lyzr_gmail_send_email");
		assert.equal(gmail.authorized, true); // credential_id present on the tool_config itself
		assert.equal(gmail.provider, "gmail"); // inferred from "gmail-<label>"
		assert.equal(gmail.toolSource, "composio");
		assert.equal(gmail.providerUuid, "6980d819100ddff45dec4e80");
		assert.deepEqual(gmail.rawToolConfig, gmailToolConfig());
		assert.equal(gmail.execSource, "agent");

		assert.equal(search.toolName, "lyzr_composio_search_search");
		assert.equal(search.authorized, true); // falls back to connected_accounts since credential_id is empty
		assert.equal(search.credentialId, "cred-search-1");

		assert.equal(stats.agentToolConfigsFound, 2);
		assert.equal(stats.agentActionsFound, 2);
		assert.equal(stats.unauthorized, 0);
	});

	it("marks a tool unauthorized when neither credential_id nor connected_accounts confirm it", async () => {
		const client = fakeClient({
			getAgent: async () => ok({ tool_configs: [gmailToolConfig({ credential_id: "" })] }),
		});

		const { tools, stats } = await discoverLyzrTools(client, baseConfig(), silentLogger());
		assert.equal(tools.length, 1);
		assert.equal(tools[0].authorized, false);
		assert.equal(stats.unauthorized, 1);
	});

	it("registers one tool per action_names entry when a single tool_config lists multiple actions", async () => {
		const client = fakeClient({
			getAgent: async () =>
				ok({
					tool_configs: [gmailToolConfig({ action_names: ["GMAIL_SEND_EMAIL", "GMAIL_FETCH_EMAILS"] })],
				}),
		});

		const { tools } = await discoverLyzrTools(client, baseConfig(), silentLogger());
		assert.equal(tools.length, 2);
		assert.ok(tools.some((t) => t.toolName === "lyzr_gmail_send_email"));
		assert.ok(tools.some((t) => t.toolName === "lyzr_gmail_fetch_emails"));
	});

	it("warns and discovers nothing when agent_id is not configured", async () => {
		const client = fakeClient();
		const logger = silentLogger();

		const { tools, stats } = await discoverLyzrTools(client, baseConfig({ agentId: undefined }), logger);
		assert.equal(tools.length, 0);
		assert.equal(stats.agentToolConfigsFound, 0);
		assert.ok(logger.messages.some((m) => /agent_id is not set/.test(m)));
		assert.equal(client.calls.some((c) => c.method === "getAgent"), false);
	});

	it("degrades gracefully when GET /v3/agents/{agent_id} fails, without throwing", async () => {
		const client = fakeClient({ getAgent: async () => fail(404, { error: "agent not found" }) });
		const { tools, stats } = await discoverLyzrTools(client, baseConfig(), silentLogger());
		assert.equal(tools.length, 0);
		assert.equal(stats.errors.length, 1);
		assert.match(stats.errors[0], /agent-1/);
	});

	it("returns no tools when the agent has an empty or missing tool_configs array", async () => {
		const client = fakeClient({ getAgent: async () => ok({ name: "Some Agent" }) });
		const { tools, stats } = await discoverLyzrTools(client, baseConfig(), silentLogger());
		assert.equal(tools.length, 0);
		assert.equal(stats.agentToolConfigsFound, 0);
	});

	it("discovers MCP server tools and treats non-oauth servers as authorized", async () => {
		const client = fakeClient({
			listMcpServers: async () => ok({ servers: [{ id: "srv-1", name: "Notion", auth_type: "api_key" }] }),
			listMcpServerTools: async (serverId: string) => {
				assert.equal(serverId, "srv-1");
				return ok({ server_name: "Notion", tools: [{ name: "search_pages", description: "Search Notion pages." }] });
			},
		});

		const { tools, stats } = await discoverLyzrTools(client, baseConfig(), silentLogger());
		assert.equal(tools.length, 1);
		assert.equal(tools[0].execSource, "mcp");
		assert.equal(tools[0].toolName, "lyzr_mcp_notion_search_pages");
		assert.equal(tools[0].authorized, true);
		assert.equal(stats.mcpServersQueried, 1);
		assert.equal(stats.mcpToolsFound, 1);
	});

	it("marks oauth MCP servers without an active token as unauthorized", async () => {
		const client = fakeClient({
			listMcpServers: async () =>
				ok({ servers: [{ id: "srv-2", name: "Linear", auth_type: "oauth", has_active_token: false }] }),
			listMcpServerTools: async () => ok({ server_name: "Linear", tools: [{ name: "create_issue" }] }),
		});

		const { tools } = await discoverLyzrTools(client, baseConfig(), silentLogger());
		assert.equal(tools.length, 1);
		assert.equal(tools[0].authorized, false);
	});

	it("skips MCP discovery entirely when include_mcp is false", async () => {
		const client = fakeClient();
		const { stats } = await discoverLyzrTools(client, baseConfig({ includeMcp: false }), silentLogger());
		assert.equal(stats.mcpServersQueried, 0);
		assert.equal(client.calls.some((c) => c.method === "listMcpServers"), false);
	});
});

// ── execute.ts ─────────────────────────────────────────────────────────

function makeTool(overrides: Partial<LyzrDiscoveredTool> = {}): LyzrDiscoveredTool {
	return {
		rawName: "GMAIL_SEND_EMAIL",
		toolName: "lyzr_gmail_send_email",
		displayName: "gmail-Akshat Gmail Integration: GMAIL_SEND_EMAIL",
		description: "Send an email via Gmail.",
		inputSchema: { properties: {} },
		execSource: "agent",
		provider: "gmail",
		toolSource: "composio",
		actionName: "GMAIL_SEND_EMAIL",
		actionNames: ["GMAIL_SEND_EMAIL"],
		providerUuid: "6980d819100ddff45dec4e80",
		credentialId: "82be4f7e-cred-1",
		rawToolConfig: gmailToolConfig(),
		authorized: true,
		...overrides,
	};
}

describe("detectAuthRequired", () => {
	it("is true for 401/403", () => {
		assert.equal(detectAuthRequired(401, {}), true);
		assert.equal(detectAuthRequired(403, {}), true);
	});

	it("is true when the error body hints at authorization", () => {
		assert.equal(detectAuthRequired(400, { error: "Gmail is not connected for this user" }), true);
		assert.equal(detectAuthRequired(500, "Please reauthenticate with the provider"), true);
	});

	it("is false for benign errors", () => {
		assert.equal(detectAuthRequired(400, { error: "recipient address is invalid" }), false);
		assert.equal(detectAuthRequired(undefined, undefined), false);
	});
});

describe("executeLyzrTool", () => {
	it("returns authorization_required without calling the API when the tool is unauthorized", async () => {
		const client = fakeClient();
		const tool = makeTool({ authorized: false, authUrl: "https://lyzr.example/authorize/gmail" });

		const result = await executeLyzrTool(client, baseConfig(), tool, {});

		assert.match(result.text, /[Aa]uthorization required/);
		assert.match(result.text, /lyzr.example\/authorize\/gmail/);
		assert.doesNotMatch(result.text, /GMAIL_APP_PASSWORD|GMAIL_USER/i);
		assert.deepEqual(result.details, {
			status: "authorization_required",
			provider: "gmail",
			tool: "lyzr_gmail_send_email",
			auth_url: "https://lyzr.example/authorize/gmail",
		});
		assert.equal(client.calls.length, 0);
	});

	it("executes an authorized agent-level tool, passing the agent's own tool_config verbatim", async () => {
		const client = fakeClient({
			executeInferenceTool: async (payload: any) => {
				assert.equal(payload.tool_name, "GMAIL_SEND_EMAIL");
				assert.deepEqual(payload.tool_configs[0], gmailToolConfig());
				return ok({ result: "Email sent to qa@example.com", trace_id: "trace-123" });
			},
		});

		const result = await executeLyzrTool(client, baseConfig(), makeTool(), { to: "qa@example.com" });

		assert.equal(result.text, "Email sent to qa@example.com");
		assert.equal((result.details as any).status, "success");
		assert.equal((result.details as any).trace_id, "trace-123");
	});

	it("maps a runtime 401 into authorization_required even if discovery thought it was authorized", async () => {
		const client = fakeClient({
			executeInferenceTool: async () => fail(401, { error: "credential expired" }),
		});

		const result = await executeLyzrTool(client, baseConfig(), makeTool({ authorized: true }), {});
		assert.equal((result.details as any).status, "authorization_required");
	});

	it("returns a redacted error result on failure, without leaking secret fields", async () => {
		const client = fakeClient({
			executeInferenceTool: async () =>
				fail(500, { error: "provider timeout", credential_id: "cred_super_secret_value" }, "provider timeout"),
		});

		const result = await executeLyzrTool(client, baseConfig(), makeTool(), {});

		assert.equal((result.details as any).status, "error");
		assert.match(result.text, /provider timeout/);
		assert.doesNotMatch(JSON.stringify(result.details), /cred_super_secret_value/);
	});

	it("routes MCP tools through executeMcpTool and reports MCP failures", async () => {
		const client = fakeClient({
			executeMcpTool: async (payload: any) => {
				assert.equal(payload.server_id, "srv-1");
				return ok({ success: false, error: "rate limited" });
			},
		});

		const tool = makeTool({ execSource: "mcp", serverId: "srv-1", provider: "notion", actionName: "search_pages" });
		const result = await executeLyzrTool(client, baseConfig(), tool, {});
		assert.equal((result.details as any).status, "error");
		assert.match(result.text, /rate limited/);
	});

	it("maps MCP authorization errors into authorization_required", async () => {
		const client = fakeClient({
			executeMcpTool: async () => ok({ success: false, error: "server is not authorized, please reconnect" }),
		});
		const tool = makeTool({ execSource: "mcp", serverId: "srv-1", provider: "notion" });
		const result = await executeLyzrTool(client, baseConfig(), tool, {});
		assert.equal((result.details as any).status, "authorization_required");
	});
});

// ── dedupe.ts ──────────────────────────────────────────────────────────

describe("buildDedupePrompt", () => {
	it("returns an empty string for no tools", () => {
		assert.equal(buildDedupePrompt([]), "");
	});

	it("names the known local duplicate for gmail", () => {
		const prompt = buildDedupePrompt([makeTool()]);
		assert.match(prompt, /lyzr_gmail_send_email/);
		assert.match(prompt, /gmail-email skill/);
	});

	it("notes unauthorized tools distinctly", () => {
		const prompt = buildDedupePrompt([makeTool({ authorized: false, provider: "slack", toolName: "lyzr_slack_send_message" })]);
		assert.match(prompt, /not yet authorized/);
	});
});

// ── config.ts ──────────────────────────────────────────────────────────

describe("resolveConfig", () => {
	it("applies defaults when given an empty config", () => {
		const cfg = resolveConfig({});
		assert.equal(cfg.apiKey, "");
		assert.equal(cfg.baseUrl, "https://agent-prod.studio.lyzr.ai");
		assert.equal(cfg.agentId, undefined);
		assert.equal(cfg.includeMcp, true);
		assert.equal(cfg.preferLyzrTools, true);
	});

	it("strips trailing slashes from base_url", () => {
		const cfg = resolveConfig({ base_url: "https://example.com/" });
		assert.equal(cfg.baseUrl, "https://example.com");
	});

	it("carries agent_id through as-is", () => {
		const cfg = resolveConfig({ agent_id: "agent-42" });
		assert.equal(cfg.agentId, "agent-42");
	});
});

// ── index.ts (registerWithClient) ───────────────────────────────────────

function fakeApi(config: Record<string, any> = {}): GitagentPluginApi & {
	registeredTools: any[];
	promptAdditions: string[];
	logMessages: string[];
} {
	const registeredTools: any[] = [];
	const promptAdditions: string[] = [];
	const logMessages: string[] = [];
	return {
		pluginId: "lyzr-tools",
		pluginDir: "/fake/plugins/lyzr-tools",
		config,
		registerTool: (def) => registeredTools.push(def),
		registerHook: () => {},
		addPrompt: (text: string) => promptAdditions.push(text),
		registerMemoryLayer: () => {},
		logger: {
			info: (m: string) => logMessages.push(`info: ${m}`),
			warn: (m: string) => logMessages.push(`warn: ${m}`),
			error: (m: string) => logMessages.push(`error: ${m}`),
		},
		registeredTools,
		promptAdditions,
		logMessages,
	};
}

describe("registerWithClient", () => {
	it("registers a lyzr_-prefixed tool per discovered action and adds dedupe prompt guidance", async () => {
		const client = fakeClient({
			getAgent: async () => ok({ tool_configs: [gmailToolConfig()] }),
		});
		const api = fakeApi();

		const tools = await registerWithClient(api, baseConfig(), client);

		assert.equal(tools.length, 1);
		assert.equal(api.registeredTools.length, 1);
		assert.equal(api.registeredTools[0].name, "lyzr_gmail_send_email");
		assert.equal(typeof api.registeredTools[0].handler, "function");
		assert.equal(api.promptAdditions.length, 1);
		assert.match(api.promptAdditions[0], /lyzr_gmail_send_email/);
		assert.match(api.promptAdditions[0], /gmail-email skill/);
	});

	it("registered tool handlers proxy execution through the client", async () => {
		const client = fakeClient({
			getAgent: async () => ok({ tool_configs: [gmailToolConfig()] }),
			executeInferenceTool: async () => ok({ result: "sent!" }),
		});
		const api = fakeApi();
		await registerWithClient(api, baseConfig(), client);

		const handlerResult = await api.registeredTools[0].handler({ to: "qa@example.com" });
		assert.equal(handlerResult.text, "sent!");
		assert.ok(client.calls.some((c) => c.method === "executeInferenceTool"));
	});

	it("registers nothing and does not throw when discovery finds no tools", async () => {
		const client = fakeClient({ getAgent: async () => ok({ tool_configs: [] }) });
		const api = fakeApi();

		const tools = await registerWithClient(api, baseConfig(), client);

		assert.equal(tools.length, 0);
		assert.equal(api.registeredTools.length, 0);
		assert.equal(api.promptAdditions.length, 0);
	});
});
