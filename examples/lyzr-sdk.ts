/**
 * Example: Using GitClaw SDK with Lyzr AI Studio
 *
 * Prerequisites:
 *   1. Set your Lyzr API key: export LYZR_API_KEY="sk-default-..."
 *   2. Set a dummy OpenAI key (needed by pi-ai): export OPENAI_API_KEY="dummy"
 *   3. Have an agent directory with agent.yaml: ~/assistant/
 *
 * Run:
 *   npx tsx examples/lyzr-sdk.ts
 */

import { query } from "../dist/exports.js";

const LYZR_API_KEY = process.env.LYZR_API_KEY;
if (!LYZR_API_KEY) {
	console.error("Error: Set LYZR_API_KEY environment variable");
	process.exit(1);
}

// Ensure pi-ai can find an API key (it checks OPENAI_API_KEY for openai-completions API)
if (!process.env.OPENAI_API_KEY) {
	process.env.OPENAI_API_KEY = LYZR_API_KEY;
}

// Your Lyzr agent ID (created via studio.lyzr.ai or the install.sh setup)
const LYZR_AGENT_ID = process.env.GITCLAW_LYZR_AGENT_ID || "your-agent-id-here";

async function main() {
	console.log("Starting GitClaw with Lyzr backend...\n");

	const result = query({
		prompt: "Hello! What can you help me with today?",
		dir: process.env.HOME + "/assistant",

		// Model format: lyzr:<agent-id>@<base-url>
		// The OpenAI SDK appends /chat/completions to the base URL
		model: `lyzr:${LYZR_AGENT_ID}@https://agent-prod.studio.lyzr.ai/v4`,

		// Optional: disable filesystem tools for a pure chat agent
		// replaceBuiltinTools: true,

		// Optional: limit turns and temperature
		constraints: {
			temperature: 0.7,
			maxTokens: 2000,
		},
		maxTurns: 5,
	});

	// Stream messages as they arrive
	for await (const msg of result) {
		switch (msg.type) {
			case "system":
				console.log(`[${msg.subtype}] ${msg.content}`);
				break;

			case "delta":
				// Real-time text streaming
				process.stdout.write(msg.content);
				break;

			case "assistant":
				// Final complete message
				console.log(`\n\nAgent: ${msg.content}`);
				if (msg.usage) {
					console.log(`  Tokens: ${msg.usage.inputTokens} in / ${msg.usage.outputTokens} out`);
				}
				break;

			case "tool_use":
				console.log(`\n[tool] ${msg.toolName}(${JSON.stringify(msg.args).slice(0, 100)})`);
				break;

			case "tool_result":
				console.log(`[result] ${msg.toolName}: ${msg.content.slice(0, 200)}`);
				break;
		}
	}

	// Print cost summary
	const costs = result.costs();
	console.log("\n--- Session Summary ---");
	console.log(`Total requests: ${costs.totalRequests}`);
	console.log(`Total tokens: ${costs.totalInputTokens} in / ${costs.totalOutputTokens} out`);
	console.log(`Total cost: $${costs.totalCostUsd.toFixed(4)}`);
}

main().catch(console.error);
