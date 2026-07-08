import { useEffect, useRef, useState } from "react";
import type {
	AppSettings,
	Artifact,
	ArtifactContent,
	PermissionRequest,
	SessionSummary,
	UIEvent,
} from "../shared/types";
import { applyEvent, reduceAll, type Item } from "./lib/transcript";
import { Sidebar, type Tab, type NavView } from "./components/Sidebar";
import { ChatView, NewSession } from "./components/ChatView";
import { ArtifactPreview } from "./components/ArtifactPreview";
import { ArtifactsView } from "./components/ArtifactsView";
import { SchedulesView } from "./components/SchedulesView";
import { SettingsModal } from "./components/SettingsModal";

export function App() {
	const [settings, setSettings] = useState<AppSettings>({ model: "openai:gpt-4o-mini", permissionMode: "plan" });
	const [settingsOpen, setSettingsOpen] = useState(false);
	const [tab, setTab] = useState<Tab>("cowork");
	const [view, setView] = useState<NavView>("session");

	const [sessions, setSessions] = useState<SessionSummary[]>([]);
	const [activeId, setActiveId] = useState<string | null>(null);
	const [items, setItems] = useState<Item[]>([]);
	const [running, setRunning] = useState(false);
	const [plan, setPlan] = useState<string | null>(null);
	const [permission, setPermission] = useState<PermissionRequest | null>(null);

	const [artifacts, setArtifacts] = useState<Artifact[]>([]);
	const [preview, setPreview] = useState<ArtifactContent | null>(null);

	const [folder, setFolder] = useState<string | null>(null);
	const [goal, setGoal] = useState("");

	const activeIdRef = useRef<string | null>(null);
	const runningIdRef = useRef<string | null>(null);
	// Streaming deltas are batched: appending to state on every token re-renders
	// the whole transcript thousands of times per response and freezes the UI.
	const deltaBuf = useRef("");
	const flushTimer = useRef<number | null>(null);
	useEffect(() => {
		activeIdRef.current = activeId;
	}, [activeId]);

	function flushDelta() {
		if (flushTimer.current !== null) {
			clearTimeout(flushTimer.current);
			flushTimer.current = null;
		}
		const t = deltaBuf.current;
		deltaBuf.current = "";
		if (t) setItems((prev) => applyEvent(prev, { type: "delta", text: t }));
	}

	// Browser-only preview seed (no Electron bridge + ?demo). Never runs in the
	// real app because window.gitagent is always defined there.
	useEffect(() => {
		if (window.gitagent || !new URLSearchParams(location.search).has("demo")) return;
		seedDemo({ setSessions, setActiveId, setItems, setArtifacts, setPreview, activeIdRef });
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, []);

	useEffect(() => {
		if (!window.gitagent) return; // preview without the Electron bridge
		void window.gitagent.getSettings().then(setSettings);
		void refreshSessions();
		const offEvent = window.gitagent.onEvent(handleEvent);
		const offPerm = window.gitagent.onPermissionRequest(setPermission);
		return () => {
			offEvent();
			offPerm();
		};
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, []);

	async function refreshSessions() {
		setSessions(await window.gitagent.listSessions());
	}
	async function refreshArtifacts(id: string) {
		setArtifacts(await window.gitagent.listArtifacts(id));
	}

	function handleEvent(e: UIEvent) {
		if (e.type === "session_started") {
			runningIdRef.current = e.session.sessionId;
			setRunning(true);
			return;
		}
		if (e.type === "session_done") {
			flushDelta();
			if (runningIdRef.current === e.sessionId) {
				runningIdRef.current = null;
				setRunning(false);
			}
			void refreshSessions();
			if (activeIdRef.current === e.sessionId) void refreshArtifacts(e.sessionId);
			return;
		}
		// Only reflect live events for the session currently in view.
		if (runningIdRef.current !== activeIdRef.current) return;

		// Batch text deltas; flush at most ~16fps.
		if (e.type === "delta") {
			deltaBuf.current += e.text;
			if (flushTimer.current === null) {
				flushTimer.current = window.setTimeout(flushDelta, 60);
			}
			return;
		}
		if (e.type === "thinking") return; // not shown; skip

		// Any structured event: flush pending text first to preserve order.
		flushDelta();
		if (e.type === "plan_proposed") setPlan(e.plan);
		if (e.type === "artifact" && activeIdRef.current) void refreshArtifacts(activeIdRef.current);
		setItems((prev) => applyEvent(prev, e));
	}

	function resetStream() {
		deltaBuf.current = "";
		if (flushTimer.current !== null) {
			clearTimeout(flushTimer.current);
			flushTimer.current = null;
		}
	}

	async function selectSession(id: string) {
		resetStream();
		setView("session");
		setActiveId(id);
		activeIdRef.current = id;
		setPlan(null);
		setPreview(null);
		const t = await window.gitagent.loadTranscript(id);
		setItems(reduceAll(t));
		void refreshArtifacts(id);
	}

	function newSession() {
		resetStream();
		setView("session");
		setActiveId(null);
		activeIdRef.current = null;
		setItems([]);
		setPlan(null);
		setPreview(null);
		setFolder(null);
		setGoal("");
	}

	async function startSession() {
		if (!folder || !goal.trim()) return;
		const g = goal;
		setItems([{ kind: "user", text: g }]);
		const entry = await window.gitagent.createSession({
			dir: folder,
			goal: g,
			model: settings.model,
			permissionMode: settings.permissionMode,
			profile: tab === "workbench" ? "cfo" : "default",
		});
		activeIdRef.current = entry.id;
		setActiveId(entry.id);
		setGoal("");
		void refreshSessions();
	}

	async function sendMessage(text: string) {
		if (!activeId) return;
		setItems((prev) => [...prev, { kind: "user", text }]);
		await window.gitagent.send(activeId, text);
	}

	async function openArtifact(path: string) {
		if (!activeId) return;
		setPreview(await window.gitagent.readArtifact(activeId, path));
	}

	const activeTitle = sessions.find((s) => s.id === activeId)?.title ?? "New session";

	return (
		<div className="app">
			<Sidebar
				sessions={sessions}
				activeId={activeId}
				tab={tab}
				view={view}
				onTab={setTab}
				onNew={newSession}
				onSelect={selectSession}
				onDelete={async (id) => {
					await window.gitagent.deleteSession(id);
					await refreshSessions();
					if (activeId === id) newSession();
				}}
				onArtifacts={() => {
					setView("artifacts");
					if (activeId) void refreshArtifacts(activeId);
				}}
				onSchedules={() => setView("schedules")}
				onSettings={() => setSettingsOpen(true)}
			/>

			<main className="main">
				{view === "schedules" ? (
					<SchedulesView />
				) : view === "artifacts" ? (
					<ArtifactsView artifacts={artifacts} onOpen={openArtifact} />
				) : activeId ? (
					<ChatView
						title={activeTitle}
						model={settings.model}
						items={items}
						running={running}
						plan={plan}
						onSend={sendMessage}
						onStop={() => window.gitagent.abort()}
						onApprovePlan={() => {
							void window.gitagent.approvePlan();
							setPlan(null);
						}}
						onRejectPlan={(fb) => {
							void window.gitagent.rejectPlan(fb);
							setPlan(null);
						}}
						onOpenArtifact={openArtifact}
						onModelClick={() => setSettingsOpen(true)}
					/>
				) : (
					<NewSession
						folder={folder}
						goal={goal}
						workbench={tab === "workbench"}
						onGoal={setGoal}
						onPick={async () => {
							const f = await window.gitagent.pickFolder();
							if (f) setFolder(f);
						}}
						onStart={startSession}
					/>
				)}
			</main>

			{preview && (
				<ArtifactPreview
					content={preview}
					onClose={() => setPreview(null)}
					onOpenSystem={() => activeId && window.gitagent.openArtifact(activeId, preview.path)}
				/>
			)}

			{permission && (
				<div className="modal-overlay">
					<div className="modal">
						<div className="modal-title">Allow tool?</div>
						<div className="modal-body">
							<code>
								{permission.toolName}({permission.target})
							</code>
						</div>
						<div className="modal-actions">
							<button
								onClick={() => {
									void window.gitagent.resolvePermission(permission.id, "deny");
									setPermission(null);
								}}
							>
								Deny
							</button>
							<button
								className="primary"
								onClick={() => {
									void window.gitagent.resolvePermission(permission.id, "allow_once");
									setPermission(null);
								}}
							>
								Allow
							</button>
						</div>
					</div>
				</div>
			)}

			{settingsOpen && (
				<SettingsModal
					settings={settings}
					onClose={() => setSettingsOpen(false)}
					onSave={async (s) => {
						await window.gitagent.saveSettings(s);
						setSettings({ ...s, keys: undefined });
						setSettingsOpen(false);
					}}
				/>
			)}
		</div>
	);
}

// ── Browser-only demo seed (presentational; never runs under Electron) ──
function seedDemo(x: {
	setSessions: (s: SessionSummary[]) => void;
	setActiveId: (id: string | null) => void;
	setItems: (i: Item[]) => void;
	setArtifacts: (a: Artifact[]) => void;
	setPreview: (c: ArtifactContent | null) => void;
	activeIdRef: { current: string | null };
}) {
	const now = Date.now();
	x.setSessions([
		{ id: "s1", dir: "/Users/you/landing-page", branch: "gitagent/session-a1b2c3d4", title: "Build a landing page + report", createdAt: now, updatedAt: now },
		{ id: "s2", dir: "/Users/you/data", branch: "gitagent/session-9f8e7d6c", title: "Clean the sales CSV and chart it", createdAt: now, updatedAt: now },
		{ id: "s3", dir: "/Users/you/notes", branch: "gitagent/session-11223344", title: "Summarize the Q3 planning docs", createdAt: now, updatedAt: now },
	]);
	x.setActiveId("s1");
	x.activeIdRef.current = "s1";
	x.setItems([
		{ kind: "user", text: "Build a small landing page and write a short markdown report of what you did." },
		{
			kind: "assistant",
			text:
				"On it. Here's the plan:\n\n" +
				"1. **Scaffold** `index.html` with a hero + feature grid\n" +
				"2. Add `styles.css` (dark theme, responsive)\n" +
				"3. Write `REPORT.md` summarizing the work\n\n" +
				"I'll keep everything inside the session folder. Starting now:",
		},
		{ kind: "tool", toolName: "write", args: { path: "index.html" }, result: "Wrote 1420 bytes to index.html", isError: false },
		{ kind: "artifact", name: "index.html", path: "index.html", akind: "html" },
		{ kind: "tool", toolName: "write", args: { path: "styles.css" }, result: "Wrote 860 bytes to styles.css", isError: false },
		{
			kind: "assistant",
			text:
				"Done. The page has a hero section, a three-column feature grid, and a call-to-action. " +
				"Here's the summary:",
		},
		{ kind: "artifact", name: "REPORT.md", path: "REPORT.md", akind: "md" },
	]);
	x.setArtifacts([
		{ path: "index.html", name: "index.html", kind: "html", updatedAt: now },
		{ path: "REPORT.md", name: "REPORT.md", kind: "md", updatedAt: now },
		{ path: "styles.css", name: "styles.css", kind: "code", updatedAt: now },
	]);
	x.setPreview({
		path: "REPORT.md",
		name: "REPORT.md",
		kind: "md",
		mime: "text/plain",
		text:
			"# Landing page — build report\n\n" +
			"Built a responsive one-page site in `index.html` with a matching `styles.css`.\n\n" +
			"## What was created\n\n" +
			"- **Hero** — headline, subhead, and a primary CTA button\n" +
			"- **Feature grid** — three cards, responsive down to mobile\n" +
			"- **Footer** — minimal, with a contact link\n\n" +
			"## Notes\n\n" +
			"All files stay inside the session folder (folder-jail). Run `open index.html` to view.\n",
	});
}
