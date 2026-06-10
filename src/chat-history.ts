import { appendFileSync, readFileSync, unlinkSync, mkdirSync, writeFileSync } from "fs";
import { join } from "path";
import type { ServerMessage } from "./adapter.js";
import { query } from "./sdk.js";

/** Types we skip — too large or ephemeral */
const SKIP_TYPES = new Set(["audio_delta", "agent_thinking"]);

function sanitizeBranch(branch: string): string {
	return branch.replace(/\//g, "__");
}

function historyDir(agentDir: string): string {
	return join(agentDir, ".gitagent", "chat-history");
}

function historyPath(agentDir: string, branch: string): string {
	return join(historyDir(agentDir), sanitizeBranch(branch) + ".jsonl");
}

export function appendMessage(agentDir: string, branch: string, msg: ServerMessage): void {
	if (SKIP_TYPES.has(msg.type)) return;
	// Skip partial transcripts
	if (msg.type === "transcript" && msg.partial) return;

	const dir = historyDir(agentDir);
	mkdirSync(dir, { recursive: true });

	const line = JSON.stringify({ ts: Date.now(), msg }) + "\n";
	appendFileSync(historyPath(agentDir, branch), line, "utf-8");
}

export function loadHistory(agentDir: string, branch: string): ServerMessage[] {
	try {
		const content = readFileSync(historyPath(agentDir, branch), "utf-8");
		const messages: ServerMessage[] = [];
		for (const line of content.split("\n")) {
			if (!line.trim()) continue;
			try {
				const entry = JSON.parse(line);
				if (entry.msg) messages.push(entry.msg);
			} catch {
				// skip malformed lines
			}
		}
		return messages;
	} catch {
		return [];
	}
}

export function deleteHistory(agentDir: string, branch: string): void {
	try {
		unlinkSync(historyPath(agentDir, branch));
	} catch {
		// file doesn't exist — that's fine
	}
}

/** Count messages for a branch (to decide when to re-summarize) */
export function getMessageCount(agentDir: string, branch: string): number {
	try {
		const content = readFileSync(historyPath(agentDir, branch), "utf-8");
		return content.split("\n").filter((l) => l.trim()).length;
	} catch {
		return 0;
	}
}

/** Summarize a branch's chat history using a lightweight query() call */
export async function summarizeHistory(agentDir: string, branch: string): Promise<string> {
	const count = getMessageCount(agentDir, branch);
	if (count < 10) return "";

	const messages = loadHistory(agentDir, branch);

	// Extract only transcripts and agent_done results for summarization
	const lines: string[] = [];
	for (const msg of messages) {
		if (msg.type === "transcript") {
			lines.push(`${msg.role}: ${msg.text}`);
		} else if (msg.type === "agent_done") {
			lines.push(`agent result: ${msg.result.slice(0, 500)}`);
		}
	}

	if (lines.length < 5) return "";

	// Truncate to last ~4000 chars to keep the summarization prompt manageable
	let transcript = lines.join("\n");
	if (transcript.length > 4000) {
		transcript = transcript.slice(-4000);
	}

	const prompt = `Summarize the following conversation in 200 words or fewer. Focus on: key decisions made, tasks completed or in progress, and current context the user cares about. Be concise and factual.\n\n${transcript}`;

	try {
		const result = query({
			prompt,
			dir: agentDir,
			maxTurns: 1,
			replaceBuiltinTools: true,
			tools: [],
			systemPrompt: "You are a concise summarizer. Output only the summary, nothing else.",
		});

		let summary = "";
		for await (const msg of result) {
			if (msg.type === "assistant" && msg.content) {
				summary += msg.content;
			}
		}

		summary = summary.trim();
		if (!summary) return "";

		// Write summary to disk
		const summaryDir = join(agentDir, ".gitagent");
		mkdirSync(summaryDir, { recursive: true });
		const safeBranch = sanitizeBranch(branch);
		const summaryPath = join(summaryDir, `chat-summary-${safeBranch}.md`);
		writeFileSync(summaryPath, summary, "utf-8");

		console.error(`[voice] Summarized ${count} messages → ${summary.length} chars`);
		return summary;
	} catch (err: any) {
		console.error(`[voice] Summarization failed: ${err.message}`);
		return "";
	}
}
