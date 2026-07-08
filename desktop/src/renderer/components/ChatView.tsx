import { useEffect, useRef, useState } from "react";
import type { Item } from "../lib/transcript";
import { CFO_SKILLS } from "../../shared/cfo";
import { CfoDashboard } from "./CfoDashboard";
import { Message } from "./Message";
import { Composer } from "./Composer";

export function ChatView(props: {
	title: string;
	model: string;
	items: Item[];
	running: boolean;
	plan: string | null;
	onSend: (text: string) => void;
	onStop: () => void;
	onApprovePlan: () => void;
	onRejectPlan: (fb: string) => void;
	onOpenArtifact: (path: string) => void;
	onModelClick: () => void;
}) {
	const scrollRef = useRef<HTMLDivElement>(null);
	useEffect(() => {
		scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
	}, [props.items, props.plan]);

	return (
		<div className="chatview">
			<header className="cv-head">
				<span className="cv-title">{props.title}</span>
				<span className="cv-caret">▾</span>
				<div className="spacer" />
				<button className="icon-btn" title="Share / export">⤴</button>
			</header>

			<div className="transcript" ref={scrollRef}>
				<div className="transcript-inner">
					{props.items.map((it, i) => (
						<Message
							key={i}
							item={it}
							streaming={props.running && i === props.items.length - 1 && it.kind === "assistant"}
							onOpenArtifact={props.onOpenArtifact}
						/>
					))}
					{props.plan && (
						<PlanBanner plan={props.plan} onApprove={props.onApprovePlan} onReject={props.onRejectPlan} />
					)}
					{props.running && !props.plan && <div className="spinner" aria-label="Agent working" />}
				</div>
			</div>

			<Composer
				model={props.model}
				running={props.running}
				onSend={props.onSend}
				onStop={props.onStop}
				onModelClick={props.onModelClick}
			/>
		</div>
	);
}

function PlanBanner(props: { plan: string; onApprove: () => void; onReject: (fb: string) => void }) {
	const [fb, setFb] = useState("");
	return (
		<div className="plan-banner">
			<div className="plan-title">Proposed plan — approval required</div>
			<pre className="plan-body">{props.plan}</pre>
			<div className="plan-actions">
				<input placeholder="Rejection feedback (optional)" value={fb} onChange={(e) => setFb(e.target.value)} />
				<button className="danger" onClick={() => props.onReject(fb)}>Reject</button>
				<button className="primary" onClick={props.onApprove}>Approve & run</button>
			</div>
		</div>
	);
}

export function NewSession(props: {
	folder: string | null;
	goal: string;
	workbench?: boolean;
	onGoal: (v: string) => void;
	onPick: () => void;
	onStart: () => void;
}) {
	return (
		<div className="new-session">
			<div className={`ns-card${props.workbench ? " ns-wide" : ""}`}>
				{props.workbench ? (
					<>
						<span className="ns-badge">CFO Workbench</span>
						<h1>CFO's Office — Lyzr Company</h1>
						<p className="muted">Autonomous financial intelligence. Point it at a finance folder and pick a journey.</p>
						<CfoDashboard />
					</>
				) : (
					<>
						<h1>Start a session</h1>
						<p className="muted">Point the agent at a folder and give it a goal. It works inside that folder only.</p>
					</>
				)}
				<button className={`pick${props.folder ? "" : " empty"}`} onClick={props.onPick}>
					<span className="pick-icon">📁</span>
					<span className="pick-label">{props.folder ?? "Choose a folder…"}</span>
				</button>
				<textarea
					placeholder={props.workbench ? "How can I help? (e.g. run the monthly close)" : "What should the agent do?"}
					value={props.goal}
					onChange={(e) => props.onGoal(e.target.value)}
				/>
				<div className="ns-row">
					<button className="primary" disabled={!props.folder || !props.goal.trim()} onClick={props.onStart}>
						{props.workbench ? "Start journey" : "Start session"}
					</button>
				</div>

				{props.workbench && (
					<div className="journeys">
						<div className="journeys-label">Agent Journeys</div>
						<div className="journeys-grid">
							{CFO_SKILLS.map((s) => (
								<button key={s.key} className="journey-card" onClick={() => props.onGoal(s.prompt)}>
									<span className="jc-icon">{s.icon}</span>
									<span className="jc-body">
										<span className="jc-name">{s.name}</span>
										<span className="jc-desc">{s.desc}</span>
									</span>
								</button>
							))}
						</div>
					</div>
				)}
			</div>
		</div>
	);
}
