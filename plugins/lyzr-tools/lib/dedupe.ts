// Phase 4: Local duplicate tool deconfliction.
//
// gitagent can't disable a local skill from a plugin, so dedupe is done the
// same way the RCA proposes: prompt guidance naming the exact Lyzr-backed
// tool to use instead, plus the "lyzr_" prefix (see lib/normalize.ts) so
// tool names can never collide outright (docs/lyzr-tool-auth-rca.md Phase 4,
// docs/lyzr-tool-bridge-test-cases.md TC-C08/TC-D04).

import type { LyzrDiscoveredTool } from "./types.ts";

// Known local bundled skills that duplicate a Lyzr-backed provider.
// Extend this as more bundled skills/tools ship with gitagent.
const KNOWN_LOCAL_DUPLICATES: Record<string, string> = {
	gmail: "the local gmail-email skill (Gmail SMTP with an app password)",
};

export function buildDedupePrompt(tools: LyzrDiscoveredTool[]): string {
	if (tools.length === 0) return "";

	const lines = [
		"### Lyzr-backed tools discovered for this session",
		"",
		"The following tools are proxied through Lyzr's pre-authorized credential vault. " +
			"Prefer them over any local skill or script that duplicates the same connected app — " +
			"the user does not need to provide app passwords, OAuth tokens, or other local credentials for these.",
		"",
	];

	for (const tool of tools) {
		const duplicate = tool.provider ? KNOWN_LOCAL_DUPLICATES[tool.provider] : undefined;
		const authNote = tool.authorized
			? ""
			: " (not yet authorized in Lyzr — calling it returns an authorization link instead of asking for local credentials)";
		const duplicateNote = duplicate ? ` Use this instead of ${duplicate}.` : "";
		lines.push(`- \`${tool.toolName}\`: ${tool.description}${authNote}${duplicateNote}`);
	}

	lines.push(
		"",
		'If a call to one of these tools returns status "authorization_required", tell the user to authorize that app in Lyzr (using the auth_url if one is provided). Never ask for local API keys, app passwords, or OAuth tokens for a tool listed above.',
	);

	return lines.join("\n");
}
