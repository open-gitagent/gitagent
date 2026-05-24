import { query } from "../dist/exports.js";

/**
 * Local Repo Mode — clone a GitHub repo, run an agent on it,
 * auto-commit changes, and push to a session branch.
 *
 * Usage:
 *   GITHUB_TOKEN=ghp_xxx npx tsx examples/local-repo.ts
 */

const REPO_URL = "https://github.com/shreyas-lyzr/agent-designer";
const TOKEN = process.env.GITHUB_TOKEN || process.env.GIT_TOKEN || "";

if (!TOKEN) {
	console.error("Set GITHUB_TOKEN or GIT_TOKEN env var");
	process.exit(1);
}

async function main() {
	console.log("Starting local repo session...\n");

	const stream = query({
		prompt: "Read the README and summarize what this project does.",
		model: "openai:gpt-4o-mini",
		repo: {
			url: REPO_URL,
			token: TOKEN,
			// dir: "/tmp/my-custom-dir",  // optional — defaults to cwd
			// session: "gitagent/session-abc123",  // resume an existing session
		},
	});

	for await (const msg of stream) {
		switch (msg.type) {
			case "delta":
				process.stdout.write(msg.content);
				break;
			case "assistant":
				console.log(`\n\n[done] model=${msg.model} tokens=${msg.usage?.totalTokens}`);
				break;
			case "tool_use":
				console.log(`\n[tool_use] ${msg.toolName}(${JSON.stringify(msg.args)})`);
				break;
			case "tool_result":
				console.log(`[tool_result] ${msg.content.slice(0, 200)}`);
				break;
			case "system":
				console.log(`[${msg.subtype}] ${msg.content}`);
				break;
		}
	}

	console.log("\nSession complete — changes committed and pushed to session branch.");
}

main().catch(console.error);