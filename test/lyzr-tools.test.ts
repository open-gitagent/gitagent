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
		providers: ["gmail", "slack"],
		includeMcp: true,
		preferLyzrTools: true,
		persistAuth: true,
		timeoutMs: 5000,
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
	it("discovers provider actions and marks authorization from connected accounts", async () => {
		const client = fakeClient({
			listConnectedAccounts: async () =>
				ok([
					{ provider: "gmail", status: "connected", credential_id: "cred-gmail-1", provider_uuid: "puid-1" },
				]),
			listProviderActions: async (providerId: string) => {
				if (providerId === "gmail") {
					return ok({ actions: [{ name: "send_email", description: "Send an email via Gmail." }] });
				}
				if (providerId === "slack") {
					return ok({ actions: [{ name: "send_message", description: "Send a Slack message." }] });
				}
				return ok({ actions: [] });
			},
		});

		const { tools, stats } = await discoverLyzrTools(client, baseConfig(), silentLogger());

		assert.equal(tools.length, 2);
		const gmail = tools.find((t) => t.provider === "gmail")!;
		const slack = tools.find((t) => t.provider === "slack")!;

		assert.equal(gmail.toolName, "lyzr_gmail_send_email");
		assert.equal(gmail.authorized, true);
		assert.equal(gmail.credentialId, "cred-gmail-1");
		assert.equal(gmail.execSource, "agent");

		assert.equal(slack.toolName, "lyzr_slack_send_message");
		assert.equal(slack.authorized, false); // not present in connected accounts
		assert.equal(stats.unauthorized, 1);
		assert.equal(stats.providerActionsFound, 2);
	});

	it("returns no tools when providers have no actions", async () => {
		const client = fakeClient({ listProviderActions: async () => ok({ actions: [] }) });
		const { tools, stats } = await discoverLyzrTools(client, baseConfig(), silentLogger());
		assert.equal(tools.length, 0);
		assert.equal(stats.providerActionsFound, 0);
		assert.equal(stats.errors.length, 0);
	});

	it("degrades gracefully when a provider call fails, without throwing", async () => {
		const client = fakeClient({
			listProviderActions: async (providerId: string) => {
				if (providerId === "gmail") return fail(401, { error: "invalid api key" });
				return ok({ actions: [{ name: "send_message", description: "Send a Slack message." }] });
			},
		});

		const { tools, stats } = await discoverLyzrTools(client, baseConfig(), silentLogger());
		assert.equal(tools.length, 1);
		assert.equal(tools[0].provider, "slack");
		assert.equal(stats.errors.length, 1);
		assert.match(stats.errors[0], /gmail/);
	});

	it("discovers MCP server tools and treats non-oauth servers as authorized", async () => {
		const client = fakeClient({
			listProviderActions: async () => ok({ actions: [] }),
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
			listProviderActions: async () => ok({ actions: [] }),
			listMcpServers: async () =>
				ok({ servers: [{ id: "srv-2", name: "Linear", auth_type: "oauth", has_active_token: false }] }),
			listMcpServerTools: async () => ok({ server_name: "Linear", tools: [{ name: "create_issue" }] }),
		});

		const { tools } = await discoverLyzrTools(client, baseConfig(), silentLogger());
		assert.equal(tools.length, 1);
		assert.equal(tools[0].authorized, false);
	});

	it("skips MCP discovery entirely when include_mcp is false", async () => {
		const client = fakeClient({ listProviderActions: async () => ok({ actions: [] }) });
		const { stats } = await discoverLyzrTools(client, baseConfig({ includeMcp: false }), silentLogger());
		assert.equal(stats.mcpServersQueried, 0);
		assert.equal(client.calls.some((c) => c.method === "listMcpServers"), false);
	});
});

// ── execute.ts ─────────────────────────────────────────────────────────

function makeTool(overrides: Partial<LyzrDiscoveredTool> = {}): LyzrDiscoveredTool {
	return {
		rawName: "send_email",
		toolName: "lyzr_gmail_send_email",
		displayName: "Gmail: Send Email",
		description: "Send an email via Gmail.",
		inputSchema: { properties: {} },
		execSource: "agent",
		provider: "gmail",
		toolSource: "gmail",
		actionName: "send_email",
		actionNames: ["send_email"],
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

	it("executes an authorized agent-level tool and returns the result", async () => {
		const client = fakeClient({
			executeInferenceTool: async (payload: any) => {
				assert.equal(payload.tool_name, "send_email");
				assert.equal(payload.tool_configs[0].tool_source, "gmail");
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
		assert.deepEqual(cfg.providers, ["gmail", "slack"]);
		assert.equal(cfg.includeMcp, true);
		assert.equal(cfg.preferLyzrTools, true);
	});

	it("splits and trims the providers list", () => {
		const cfg = resolveConfig({ providers: " gmail , slack ,notion" });
		assert.deepEqual(cfg.providers, ["gmail", "slack", "notion"]);
	});

	it("strips trailing slashes from base_url", () => {
		const cfg = resolveConfig({ base_url: "https://example.com/" });
		assert.equal(cfg.baseUrl, "https://example.com");
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
			listConnectedAccounts: async () => ok([{ provider: "gmail", status: "connected" }]),
			listProviderActions: async (providerId: string) =>
				providerId === "gmail"
					? ok({ actions: [{ name: "send_email", description: "Send an email via Gmail." }] })
					: ok({ actions: [] }),
		});
		const api = fakeApi();

		const tools = await registerWithClient(api, baseConfig({ providers: ["gmail"] }), client);

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
			listConnectedAccounts: async () => ok([{ provider: "gmail", status: "connected" }]),
			listProviderActions: async () => ok({ actions: [{ name: "send_email", description: "Send email." }] }),
			executeInferenceTool: async () => ok({ result: "sent!" }),
		});
		const api = fakeApi();
		await registerWithClient(api, baseConfig({ providers: ["gmail"] }), client);

		const handlerResult = await api.registeredTools[0].handler({ to: "qa@example.com" });
		assert.equal(handlerResult.text, "sent!");
		assert.ok(client.calls.some((c) => c.method === "executeInferenceTool"));
	});

	it("registers nothing and does not throw when discovery finds no tools", async () => {
		const client = fakeClient({ listProviderActions: async () => ok({ actions: [] }) });
		const api = fakeApi();

		const tools = await registerWithClient(api, baseConfig(), client);

		assert.equal(tools.length, 0);
		assert.equal(api.registeredTools.length, 0);
		assert.equal(api.promptAdditions.length, 0);
	});
});
