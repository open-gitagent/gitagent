import { appendFileSync, readFileSync, unlinkSync, mkdirSync } from "fs";
import { join } from "path";
import type { ServerMessage } from "./adapter.js";

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
