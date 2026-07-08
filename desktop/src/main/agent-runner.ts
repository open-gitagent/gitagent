import type { WebContents } from "electron";
import { basename } from "path";
import { query, initLocalFolderSession } from "@open-gitagent/gitagent";
import type { Query, GCMessage, GCUserMessage, PermissionDecision } from "@open-gitagent/gitagent";
import type {
	StartSessionOptions,
	SessionSummary,
	UIEvent,
	PermissionReply,
	PermissionMode,
	AgentProfile,
} from "../shared/types";
import { CFO_SYSTEM_PROMPT } from "../shared/cfo";
import * as sessions from "./sessions";
import * as artifacts from "./artifacts";

// One active run at a time. Follow-up turns feed query() via an async-iterable queue.

interface PromptQueue {
	iterable: AsyncIterable<GCUserMessage>;
	push: (text: string) => void;
	end: () => void;
}

function makePromptQueue(): PromptQueue {
	const buffer: GCUserMessage[] = [];
	let resolve: ((r: IteratorResult<GCUserMessage>) => void) | null = null;
	let done = false;
	const push = (text: string) => {
		const msg: GCUserMessage = { type: "user", content: text };
		if (resolve) {
			resolve({ value: msg, done: false });
			resolve = null;
		} else buffer.push(msg);
	};
	const end = () => {
		done = true;
		if (resolve) {
			resolve({ value: undefined as never, done: true });
			resolve = null;
		}
	};
	const iterable: AsyncIterable<GCUserMessage> = {
		[Symbol.asyncIterator]: () => ({
			next() {
				if (buffer.length) return Promise.resolve({ value: buffer.shift()!, done: false });
				if (done) return Promise.resolve({ value: undefined as never, done: true });
				return new Promise((r) => {
					resolve = r;
				});
			},
		}),
	};
	return { iterable, push, end };
}

interface ActiveRun {
	id: string;
	dir: string;
	branch: string;
	model?: string;
	permissionMode?: PermissionMode;
	queue: PromptQueue;
	q: Query;
}

let active: ActiveRun | null = null;
const pendingPermissions = new Map<string, (d: PermissionDecision) => void>();
const pendingArtifacts = new Map<string, string>(); // toolCallId → path
let permCounter = 0;

function emit(wc: WebContents, id: string, e: UIEvent): void {
	wc.send("agent:event", e);
	sessions.appendTranscript(id, e);
}

function translate(msg: GCMessage): UIEvent | null {
	switch (msg.type) {
		case "delta":
			return msg.deltaType === "thinking"
				? { type: "thinking", text: msg.content }
				: { type: "delta", text: msg.content };
		case "assistant":
			return { type: "assistant_done", costUsd: msg.usage?.costUsd };
		case "tool_use":
			return { type: "tool_call", toolName: msg.toolName, args: msg.args };
		case "tool_result":
			return { type: "tool_result", toolName: msg.toolName, content: msg.content, isError: msg.isError };
		case "plan_proposed":
			return { type: "plan_proposed", plan: msg.plan };
		case "system":
			return { type: "system", subtype: msg.subtype, content: msg.content };
		default:
			return null;
	}
}

function startRun(
	wc: WebContents,
	id: string,
	dir: string,
	branch: string,
	prompt: string,
	model?: string,
	permissionMode?: PermissionMode,
	profile?: AgentProfile,
): void {
	if (active) {
		active.queue.end();
		active.q.abort();
		active = null;
	}

	// Resume the branch (or create it) and jail to the folder.
	const session = initLocalFolderSession(dir, { session: branch });
	const queue = makePromptQueue();
	queue.push(prompt);

	const q = query({
		prompt: queue.iterable,
		dir: session.dir,
		rootDir: session.dir,
		model,
		permissionMode,
		// CFO Workbench: append the CFO persona + skills on top of the base prompt.
		systemPromptSuffix: profile === "cfo" ? CFO_SYSTEM_PROMPT : undefined,
		canUseTool: (toolName, _args, ctx) =>
			new Promise<PermissionDecision>((resolve) => {
				const pid = `perm-${++permCounter}`;
				pendingPermissions.set(pid, resolve);
				wc.send("permission:request", { id: pid, toolName, target: ctx.target, isReadOnly: ctx.isReadOnly });
			}),
	});

	active = { id, dir: session.dir, branch, model, permissionMode, queue, q };
	sessions.update(id, { updatedAt: Date.now() });

	// Signal the renderer that this session is now the running one. Without this
	// the renderer's live-event gate (runningId === activeId) drops every delta,
	// assistant token, and error for the run. Live-only — not a transcript item.
	wc.send("agent:event", {
		type: "session_started",
		session: { sessionId: id, branch, dir: session.dir },
	} satisfies UIEvent);

	void (async () => {
		// Split live streaming (wc.send, every token) from persistence (append to
		// disk). Persisting per-token blocked the main thread; instead accumulate
		// the streamed text and persist it once per assistant message.
		const send = (e: UIEvent) => wc.send("agent:event", e);
		const persist = (e: UIEvent) => sessions.appendTranscript(id, e);
		let liveText = "";
		try {
			for await (const msg of q) {
				// Correlate write/edit results into artifacts.
				if ((msg.type === "tool_use") && (msg.toolName === "write" || msg.toolName === "edit")) {
					const p = (msg.args?.path as string) ?? "";
					if (p) pendingArtifacts.set(msg.toolCallId, p);
				}
				if (msg.type === "tool_result" && pendingArtifacts.has(msg.toolCallId)) {
					const p = pendingArtifacts.get(msg.toolCallId)!;
					pendingArtifacts.delete(msg.toolCallId);
					if (!msg.isError) {
						artifacts.record(id, p);
						const ae: UIEvent = { type: "artifact", name: basename(p), path: p, kind: artifacts.kindFor(p) };
						send(ae);
						persist(ae);
					}
				}

				if (msg.type === "delta") {
					if (msg.deltaType === "thinking") {
						send({ type: "thinking", text: msg.content });
					} else {
						liveText += msg.content;
						send({ type: "delta", text: msg.content }); // live only, no disk write
					}
					continue;
				}
				if (msg.type === "assistant") {
					if (liveText) {
						persist({ type: "delta", text: liveText }); // one write per message
						liveText = "";
					}
					send({ type: "assistant_done", costUsd: msg.usage?.costUsd });
					continue;
				}

				const e = translate(msg);
				if (e) {
					send(e);
					persist(e);
				}
			}
		} catch (err) {
			emit(wc, id, {
				type: "system",
				subtype: "error",
				content: err instanceof Error ? err.message : String(err),
			});
		} finally {
			wc.send("agent:event", { type: "session_done", sessionId: id } satisfies UIEvent);
			// NOTE: no synchronous session.finalize() here — git add/commit via
			// execSync blocks the main thread (freezes the UI) on large repos.
			// Files are already on disk; versioning can be done out-of-band.
			sessions.update(id, { updatedAt: Date.now() });
			if (active?.id === id) active = null;
		}
	})();
}

export function createSession(wc: WebContents, opts: StartSessionOptions): SessionSummary {
	const session = initLocalFolderSession(opts.dir);
	const now = Date.now();
	const entry: SessionSummary = {
		id: session.sessionId,
		dir: session.dir,
		branch: session.branch,
		title: sessions.titleFrom(opts.goal),
		model: opts.model,
		permissionMode: opts.permissionMode,
		profile: opts.profile,
		createdAt: now,
		updatedAt: now,
	};
	sessions.upsert(entry);
	sessions.appendTranscript(entry.id, { type: "user", text: opts.goal }); // for replay; renderer shows optimistically
	startRun(wc, entry.id, session.dir, session.branch, opts.goal, opts.model, opts.permissionMode, opts.profile);
	return entry;
}

export function send(wc: WebContents, sessionId: string, text: string): void {
	sessions.appendTranscript(sessionId, { type: "user", text });
	if (active?.id === sessionId) {
		active.queue.push(text);
		return;
	}
	// Resume an idle/switched session with this message.
	const s = sessions.get(sessionId);
	if (!s) return;
	startRun(wc, s.id, s.dir, s.branch, text, s.model, s.permissionMode, s.profile);
}

export function abort(): void {
	if (active) {
		active.queue.end();
		active.q.abort();
	}
}

export function approvePlan(mode?: PermissionMode): void {
	active?.q.approvePlan(mode ? { mode } : undefined);
}

export function rejectPlan(feedback: string): void {
	active?.q.rejectPlan(feedback);
}

export function resolvePermission(id: string, reply: PermissionReply): void {
	const resolve = pendingPermissions.get(id);
	if (!resolve) return;
	pendingPermissions.delete(id);
	resolve(reply === "deny" ? { behavior: "deny", message: "Denied by user." } : { behavior: "allow" });
}
