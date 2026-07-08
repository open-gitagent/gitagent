// Shared DTOs across main / preload / renderer. Dependency-free.

export type PermissionMode = "default" | "plan" | "acceptEdits" | "bypassPermissions";

/** Agent persona for a session. "cfo" = the CFO Workbench profile. */
export type AgentProfile = "default" | "cfo";

export interface StartSessionOptions {
	dir: string;
	goal: string;
	model?: string;
	permissionMode?: PermissionMode;
	profile?: AgentProfile;
}

export interface SessionInfo {
	sessionId: string;
	branch: string;
	dir: string;
}

/** Registry entry shown in the Recents list. */
export interface SessionSummary {
	id: string;
	dir: string;
	branch: string;
	title: string;
	model?: string;
	permissionMode?: PermissionMode;
	profile?: AgentProfile;
	createdAt: number;
	updatedAt: number;
}

export type ArtifactKind = "md" | "code" | "image" | "pdf" | "html" | "other";

export interface Artifact {
	path: string; // relative to the session dir
	name: string;
	kind: ArtifactKind;
	updatedAt: number;
}

export interface ArtifactContent {
	path: string;
	name: string;
	kind: ArtifactKind;
	mime: string;
	text?: string; // for md/code/html/other-text
	base64?: string; // for image/pdf/binary
}

/** Events streamed from main → renderer, and persisted for replay. */
export type UIEvent =
	| { type: "user"; text: string }
	| { type: "delta"; text: string }
	| { type: "thinking"; text: string }
	| { type: "assistant_done"; costUsd?: number }
	| { type: "tool_call"; toolName: string; args: Record<string, unknown> }
	| { type: "tool_result"; toolName: string; content: string; isError: boolean }
	| { type: "artifact"; name: string; path: string; kind: ArtifactKind }
	| { type: "plan_proposed"; plan: string }
	| { type: "system"; subtype: string; content: string }
	| { type: "session_started"; session: SessionInfo }
	| { type: "session_done"; sessionId: string };

export interface PermissionRequest {
	id: string;
	toolName: string;
	target: string;
	isReadOnly: boolean;
}

export type PermissionReply = "allow_once" | "allow_always" | "deny";

export interface AppSettings {
	model: string;
	keys?: Record<string, string>;
	permissionMode: PermissionMode;
	/** OpenAI-compatible gateway base URL → GITAGENT_MODEL_BASE_URL (e.g. http://localhost:8090/v1). */
	baseUrl?: string;
}

/** window.gitagent — exposed by the preload bridge. */
export interface GitagentApi {
	pickFolder(): Promise<string | null>;

	// sessions
	listSessions(): Promise<SessionSummary[]>;
	createSession(opts: StartSessionOptions): Promise<SessionSummary>;
	loadTranscript(id: string): Promise<UIEvent[]>;
	renameSession(id: string, title: string): Promise<void>;
	deleteSession(id: string): Promise<void>;

	// active run
	send(sessionId: string, text: string): Promise<void>;
	abort(): Promise<void>;
	approvePlan(mode?: PermissionMode): Promise<void>;
	rejectPlan(feedback: string): Promise<void>;
	resolvePermission(id: string, reply: PermissionReply): Promise<void>;

	// artifacts
	listArtifacts(id: string): Promise<Artifact[]>;
	readArtifact(id: string, relPath: string): Promise<ArtifactContent>;
	openArtifact(id: string, relPath: string): Promise<void>;

	// settings
	getSettings(): Promise<AppSettings>;
	saveSettings(settings: AppSettings): Promise<void>;

	// streams
	onEvent(cb: (e: UIEvent) => void): () => void;
	onPermissionRequest(cb: (r: PermissionRequest) => void): () => void;
}
