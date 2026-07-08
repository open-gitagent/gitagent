import { app } from "electron";
import { join } from "path";
import { readFileSync, writeFileSync, appendFileSync, mkdirSync, unlinkSync } from "fs";
import type { SessionSummary, UIEvent } from "../shared/types";

// Global session registry (userData/sessions.json) + per-session transcript
// (userData/transcripts/<id>.jsonl). Sessions span folders, so the index is
// app-global, mirroring Cowork's Recents.

function root(): string {
	return app.getPath("userData");
}
function registryPath(): string {
	return join(root(), "sessions.json");
}
function transcriptsDir(): string {
	return join(root(), "transcripts");
}
function transcriptPath(id: string): string {
	return join(transcriptsDir(), `${id}.jsonl`);
}

export function list(): SessionSummary[] {
	try {
		return JSON.parse(readFileSync(registryPath(), "utf-8")) as SessionSummary[];
	} catch {
		return [];
	}
}

function writeAll(all: SessionSummary[]): void {
	mkdirSync(root(), { recursive: true });
	writeFileSync(registryPath(), JSON.stringify(all, null, 2), "utf-8");
}

export function get(id: string): SessionSummary | null {
	return list().find((s) => s.id === id) ?? null;
}

export function upsert(entry: SessionSummary): void {
	const all = list();
	const i = all.findIndex((s) => s.id === entry.id);
	if (i >= 0) all[i] = entry;
	else all.unshift(entry);
	writeAll(all);
}

export function update(id: string, patch: Partial<SessionSummary>): void {
	const all = list();
	const i = all.findIndex((s) => s.id === id);
	if (i < 0) return;
	all[i] = { ...all[i], ...patch };
	writeAll(all);
}

export function remove(id: string): void {
	writeAll(list().filter((s) => s.id !== id));
	try {
		unlinkSync(transcriptPath(id));
	} catch {
		/* no transcript */
	}
}

/** Append one event to the session transcript (cheap append; registry not touched). */
export function appendTranscript(id: string, e: UIEvent): void {
	mkdirSync(transcriptsDir(), { recursive: true });
	appendFileSync(transcriptPath(id), JSON.stringify(e) + "\n", "utf-8");
}

export function loadTranscript(id: string): UIEvent[] {
	try {
		return readFileSync(transcriptPath(id), "utf-8")
			.split("\n")
			.filter(Boolean)
			.map((l) => JSON.parse(l) as UIEvent);
	} catch {
		return [];
	}
}

export function titleFrom(goal: string): string {
	const first = goal.trim().split("\n")[0].trim();
	return first.length > 60 ? first.slice(0, 60) + "…" : first || "New session";
}
