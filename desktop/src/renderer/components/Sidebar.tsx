import type { SessionSummary } from "../../shared/types";
import logo from "../assets/gitagent-logo.png";

export type Tab = "chat" | "cowork" | "workbench";

const TAB_LABELS: Record<Tab, string> = {
	chat: "Chat",
	cowork: "Cowork",
	workbench: "Workbench",
};
export type NavView = "session" | "artifacts" | "schedules";

export function Sidebar(props: {
	sessions: SessionSummary[];
	activeId: string | null;
	tab: Tab;
	view: NavView;
	onTab: (t: Tab) => void;
	onNew: () => void;
	onSelect: (id: string) => void;
	onDelete: (id: string) => void;
	onArtifacts: () => void;
	onSchedules: () => void;
	onSettings: () => void;
}) {
	return (
		<aside className="sidebar">
			<div className="sb-top">
				<button className="icon-btn" title="Toggle sidebar">◧</button>
				<button className="icon-btn" title="Search">⌕</button>
			</div>

			<div className="segmented">
				{(["chat", "cowork", "workbench"] as Tab[]).map((t) => (
					<button key={t} className={`seg ${props.tab === t ? "active" : ""}`} onClick={() => props.onTab(t)}>
						{TAB_LABELS[t]}
					</button>
				))}
			</div>

			<nav className="sb-nav">
				<button className="nav-row" onClick={props.onNew}>
					<span className="ni">＋</span> New session
				</button>
				<button className="nav-row" disabled>
					<span className="ni">▤</span> Projects
				</button>
				<button className={`nav-row ${props.view === "artifacts" ? "active" : ""}`} onClick={props.onArtifacts}>
					<span className="ni">◈</span> Artifacts
				</button>
				<button className={`nav-row ${props.view === "schedules" ? "active" : ""}`} onClick={props.onSchedules}>
					<span className="ni">◷</span> Schedule Jobs
				</button>
				<button className="nav-row" onClick={props.onSettings}>
					<span className="ni">⚙</span> Customize
				</button>
			</nav>

			<div className="recents">
				<div className="recents-label">Recents</div>
				<div className="recents-list">
					{props.sessions.length === 0 && <div className="recents-empty">No sessions yet</div>}
					{props.sessions.map((s) => (
						<div
							key={s.id}
							className={`recent ${props.activeId === s.id && props.view === "session" ? "active" : ""}`}
							onClick={() => props.onSelect(s.id)}
							title={`${s.dir}\n${s.branch}`}
						>
							<span className="recent-title">{s.title}</span>
							<button
								className="recent-x"
								title="Delete"
								onClick={(e) => {
									e.stopPropagation();
									props.onDelete(s.id);
								}}
							>
								×
							</button>
						</div>
					))}
				</div>
			</div>

			<div className="sb-bottom">
				<div className="logged-user">
					<div className="lu-label">Logged in User</div>
					<div className="lu-name">Shreyas</div>
					<div className="lu-role">CFO &amp; Finance Team</div>
				</div>
				<div className="account">
					<img className="avatar-img" src={logo} alt="GitAgent" />
					<span className="acct-name">GitAgent</span>
					<span className="acct-plan">local</span>
				</div>
			</div>
		</aside>
	);
}
