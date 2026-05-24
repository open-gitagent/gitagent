import { query, tool } from "../dist/exports.js";

// A custom tool the agent can call
const greet = tool("greet", "Greet someone by name", {
	properties: { name: { type: "string", description: "Name to greet" } },
	required: ["name"],
}, async (args) => `Hello, ${args.name}! Welcome to Gitagent.`);

async function main() {
	console.log("Starting SDK demo...\n");

	for await (const msg of query({
		prompt: "Use the greet tool to greet Zeus, then say something short and fun.",
		dir: process.cwd(),
		model: "openai:gpt-4o-mini",
		tools: [greet],
		hooks: {
			preToolUse: async (ctx) => {
				console.log(`[hook] tool "${ctx.toolName}" called with:`, ctx.args);
				return { action: "allow" };
			},
		},
	})) {
		switch (msg.type) {
			case "delta":
				process.stdout.write(msg.content);
				break;
			case "assistant":
				if (msg.errorMessage) {
					console.error(`\n[error] ${msg.errorMessage}`);
				} else {
					console.log(`\n\n[done] model=${msg.model} tokens=${msg.usage?.totalTokens}`);
				}
				break;
			case "tool_use":
				console.log(`\n[tool_use] ${msg.toolName}(${JSON.stringify(msg.args)})`);
				break;
			case "tool_result":
				console.log(`[tool_result] ${msg.content}`);
				break;
			case "system":
				console.log(`[${msg.subtype}] ${msg.content}`);
				break;
		}
	}
}

main().catch(console.error);
