/**
 * Live smoke test for Compactor + transformContext against a small OpenAI model.
 * Usage: node --experimental-strip-types examples/live-compaction.ts
 * Requires OPENAI_API_KEY in the environment.
 */
import { Compactor, createTransformContext } from "../dist/compactor.js";
import type { AgentMessage } from "@mariozechner/pi-agent-core";
import { Agent } from "@mariozechner/pi-agent-core";
import { getModel } from "@mariozechner/pi-ai";

const MODEL = process.env.GITAGENT_LIVE_MODEL || "gpt-4o-mini";

function user(text: string): AgentMessage {
	return { role: "user", content: text, timestamp: Date.now() };
}

function toolResult(id: string, name: string, content: string): AgentMessage {
	return {
		role: "toolResult",
		toolCallId: id,
		toolName: name,
		content: [{ type: "text", text: content }],
		isError: false,
		timestamp: Date.now(),
	};
}

function assistantTool(id: string, name: string): AgentMessage {
	return {
		role: "assistant",
		content: [{ type: "toolCall", id, name, arguments: { path: "big.txt" } }],
		api: "openai-completions",
		provider: "openai",
		model: MODEL,
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "toolUse",
		timestamp: Date.now(),
	};
}

function buildOversized(): AgentMessage[] {
	const big = "LINE_OF_NOISE ".repeat(4000); // ~56k chars each
	const msgs: AgentMessage[] = [user("Investigate the flaky auth redirect.")];
	for (let i = 0; i < 5; i++) {
		msgs.push(assistantTool(`t${i}`, "read"));
		msgs.push(toolResult(`t${i}`, "read", `file-${i}.ts\n${big}`));
	}
	msgs.push(user("Summarize what you found in one short sentence."));
	return msgs;
}

async function openaiSummarizer(prompt: string): Promise<string> {
	const res = await fetch("https://api.openai.com/v1/chat/completions", {
		method: "POST",
		headers: {
			Authorization: `Bearer ${process.env.OPENAI_API_KEY}`,
			"Content-Type": "application/json",
		},
		body: JSON.stringify({
			model: MODEL,
			temperature: 0,
			max_tokens: 256,
			messages: [
				{
					role: "system",
					content: "You are a precise conversation summarizer for an autonomous agent.",
				},
				{ role: "user", content: prompt },
			],
		}),
	});
	if (!res.ok) {
		const body = await res.text();
		throw new Error(`summarizer HTTP ${res.status}: ${body.slice(0, 400)}`);
	}
	const json = (await res.json()) as {
		choices?: { message?: { content?: string } }[];
	};
	return json.choices?.[0]?.message?.content?.trim() || "";
}

async function main() {
	if (!process.env.OPENAI_API_KEY) {
		console.error("OPENAI_API_KEY not set");
		process.exit(1);
	}

	const msgs = buildOversized();
	const c = new Compactor({
		contextWindow: 4000, // force compact
		summarizer: openaiSummarizer,
	});

	console.log(`model=${MODEL}`);
	console.log(
		`before: estimate=${c.estimateTokens(msgs)} maxTokens=${c.maxTokens} msgs=${msgs.length}`,
	);
	assertOver(c.estimateTokens(msgs), c.maxTokens);

	const compacted = await c.compact(msgs);
	console.log(
		`after compact: estimate=${c.estimateTokens(compacted)} msgs=${compacted.length}`,
	);
	const head = compacted[0] as { role: string; content: string };
	console.log(`head.role=${head.role}`);
	console.log(`head.preview=${String(head.content).slice(0, 240).replace(/\n/g, " ")}`);
	if (c.estimateTokens(compacted) > c.maxTokens) {
		throw new Error("compact failed to get under budget");
	}
	if (!String(head.content).includes("Summary of the earlier conversation")) {
		throw new Error("expected summary head message");
	}

	// Live Agent turn: tiny window Compactor + real gpt-4o-mini via transformContext
	const model = getModel("openai", MODEL as any);
	if (!model) throw new Error(`getModel(openai, ${MODEL}) returned undefined`);

	const agentCompactor = new Compactor({
		contextWindow: 4000,
		summarizer: openaiSummarizer,
	});
	const agent = new Agent({
		initialState: {
			systemPrompt: "Reply in one short sentence. Do not call tools.",
			model,
			tools: [],
		},
		transformContext: createTransformContext(agentCompactor),
	});
	agent.state.messages = buildOversized().slice(0, -1); // leave room for new prompt

	console.log("\n--- live Agent.prompt with transformContext ---");
	let sawAssistant = false;
	let errorMsg = "";
	agent.subscribe((ev) => {
		if (ev.type === "message_end" && (ev.message as any).role === "assistant") {
			sawAssistant = true;
			const m = ev.message as any;
			console.log(
				`assistant stop=${m.stopReason} input=${m.usage?.input} output=${m.usage?.output}`,
			);
			if (m.errorMessage) {
				errorMsg = m.errorMessage;
				console.error(`errorMessage=${m.errorMessage}`);
			} else {
				const text = (m.content || [])
					.filter((b: any) => b.type === "text")
					.map((b: any) => b.text)
					.join("");
				console.log(`text=${text.slice(0, 300)}`);
			}
		}
	});

	await agent.prompt("In one short sentence, what was the investigation about?");
	if (!sawAssistant) throw new Error("no assistant message");
	if (errorMsg) throw new Error(`agent error: ${errorMsg}`);

	console.log("\nLIVE COMPACTION OK");
}

function assertOver(est: number, max: number) {
	if (est <= max) throw new Error(`expected over budget, got ${est} <= ${max}`);
}

main().catch((err) => {
	console.error("\nLIVE COMPACTION FAILED", err);
	process.exit(1);
});
