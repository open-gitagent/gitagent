// lyzr-tools: Lyzr Tool Bridge plugin.
//
// Discovers tools already authorized in Lyzr (Gmail, Slack, and other
// connected apps) and registers them as gitagent tools that execute
// server-side through Lyzr, instead of requiring the user to separately
// authorize/configure them locally.
//
// See docs/lyzr-tool-auth-rca.md for the root-cause analysis and full
// design, and docs/lyzr-tool-bridge-test-cases.md for the acceptance
// criteria this implementation targets.

import type { GitagentPluginApi } from "../../src/plugin-sdk.ts";
import { createLyzrClient, type LyzrClient } from "./lib/client.ts";
import { resolveConfig } from "./lib/config.ts";
import { buildDedupePrompt } from "./lib/dedupe.ts";
import { discoverLyzrTools } from "./lib/discover.ts";
import { executeLyzrTool } from "./lib/execute.ts";
import type { LyzrDiscoveredTool, ResolvedConfig } from "./lib/types.ts";

export async function register(api: GitagentPluginApi): Promise<void> {
	const config = resolveConfig(api.config);

	if (!config.apiKey) {
		api.logger.warn(
			'api_key is not set (config "api_key" / env "LYZR_API_KEY"); lyzr-tools will not discover or register any tools.',
		);
		return;
	}

	const client = createLyzrClient(config);
	await registerWithClient(api, config, client);
}

export default register;

/**
 * Core registration logic, factored out from register() so it can be
 * exercised in tests with a fake LyzrClient instead of real network calls.
 */
export async function registerWithClient(
	api: GitagentPluginApi,
	config: ResolvedConfig,
	client: LyzrClient,
): Promise<LyzrDiscoveredTool[]> {
	let tools: LyzrDiscoveredTool[] = [];
	try {
		const discovered = await discoverLyzrTools(client, config, api.logger);
		tools = discovered.tools;

		api.logger.info(
			`Discovered ${tools.length} Lyzr-backed tool(s) across ${discovered.stats.providersQueried} provider(s) and ${discovered.stats.mcpServersQueried} MCP server(s); ${discovered.stats.unauthorized} not yet authorized.`,
		);
		if (discovered.stats.errors.length > 0) {
			api.logger.warn(`Some discovery calls failed: ${discovered.stats.errors.join("; ")}`);
		}
	} catch (err: any) {
		// Discovery must never take down the rest of plugin loading.
		api.logger.error(`Discovery failed: ${err?.message ?? err}`);
		return [];
	}

	for (const tool of tools) {
		api.registerTool({
			name: tool.toolName,
			description: buildToolDescription(tool),
			inputSchema: tool.inputSchema,
			handler: async (args: Record<string, unknown>) => {
				try {
					return await executeLyzrTool(client, config, tool, args ?? {});
				} catch (err: any) {
					api.logger.error(`Execution of "${tool.toolName}" failed: ${err?.message ?? err}`);
					return {
						text: `Lyzr tool "${tool.displayName}" failed unexpectedly.`,
						details: { status: "error", tool: tool.toolName },
					};
				}
			},
		});
	}

	if (config.preferLyzrTools && tools.length > 0) {
		api.addPrompt(buildDedupePrompt(tools));
	}

	return tools;
}

function buildToolDescription(tool: LyzrDiscoveredTool): string {
	const authNote = tool.authorized
		? " Executed through Lyzr's pre-authorized credentials."
		: " Not yet authorized in Lyzr; calling it returns an authorization link.";
	return `${tool.description}${tool.description.endsWith(".") ? "" : "."}${authNote}`;
}
