import { useState } from "react";

// Frontend-only prototype: schedules are persisted to localStorage and do not
// run yet. The shape mirrors what a real scheduler (cron) would need so the
// backend wiring is a drop-in later.

export type Frequency = "hourly" | "daily" | "weekly" | "monthly";

export interface Schedule {
	id: string;
	name: string;
	prompt: string;
	frequency: Frequency;
	time: string; // "HH:MM"
	weekday: number; // 0–6, for weekly
	dayOfMonth: number; // 1–31, for monthly
	enabled: boolean;
	createdAt: number;
}

const STORAGE_KEY = "gitagent.schedules";
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const FREQS: Frequency[] = ["hourly", "daily", "weekly", "monthly"];

function loadSchedules(): Schedule[] {
	try {
		return JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "[]") as Schedule[];
	} catch {
		return [];
	}
}
function saveSchedules(s: Schedule[]): void {
	localStorage.setItem(STORAGE_KEY, JSON.stringify(s));
}

function blankSchedule(): Schedule {
	return {
		id: crypto.randomUUID(),
		name: "",
		prompt: "",
		frequency: "daily",
		time: "09:00",
		weekday: 1,
		dayOfMonth: 1,
		enabled: true,
		createdAt: Date.now(),
	};
}

function summarize(s: Schedule): string {
	const mins = s.time.split(":")[1] ?? "00";
	switch (s.frequency) {
		case "hourly":
			return `Every hour at :${mins}`;
		case "daily":
			return `Daily at ${s.time}`;
		case "weekly":
			return `Weekly on ${WEEKDAYS[s.weekday]} at ${s.time}`;
		case "monthly":
			return `Monthly on day ${s.dayOfMonth} at ${s.time}`;
	}
}

export function SchedulesView() {
	const [schedules, setSchedules] = useState<Schedule[]>(loadSchedules);
	const [editing, setEditing] = useState<Schedule | null>(null);

	function persist(next: Schedule[]) {
		setSchedules(next);
		saveSchedules(next);
	}
	function upsert(s: Schedule) {
		const exists = schedules.some((x) => x.id === s.id);
		persist(exists ? schedules.map((x) => (x.id === s.id ? s : x)) : [s, ...schedules]);
		setEditing(null);
	}
	function remove(id: string) {
		persist(schedules.filter((s) => s.id !== id));
	}
	function toggle(id: string) {
		persist(schedules.map((s) => (s.id === id ? { ...s, enabled: !s.enabled } : s)));
	}

	return (
		<div className="schedules-view">
			<div className="sv-head">
				<h1>Scheduled Jobs</h1>
				<button className="primary" onClick={() => setEditing(blankSchedule())}>
					＋ New schedule
				</button>
			</div>
			<p className="muted sv-note">Prototype — schedules are saved locally and don't run yet.</p>

			{schedules.length === 0 ? (
				<div className="muted sv-empty">
					No schedules yet. Create one to prompt the agent on a recurring time.
				</div>
			) : (
				<div className="sched-list">
					{schedules.map((s) => (
						<div className={`sched-card${s.enabled ? "" : " off"}`} key={s.id}>
							<div className="sc-main">
								<div className="sc-top">
									<span className="sc-name">{s.name || "Untitled schedule"}</span>
									<span className="sc-when">{summarize(s)}</span>
								</div>
								<div className="sc-prompt">{s.prompt || "No prompt set"}</div>
							</div>
							<div className="sc-actions">
								<button
									className={`sw-toggle${s.enabled ? " on" : ""}`}
									title={s.enabled ? "Enabled — click to pause" : "Paused — click to enable"}
									onClick={() => toggle(s.id)}
								>
									<span className="sw-knob" />
								</button>
								<button className="sc-btn" onClick={() => setEditing(s)}>
									Edit
								</button>
								<button className="sc-btn danger" onClick={() => remove(s.id)}>
									Delete
								</button>
							</div>
						</div>
					))}
				</div>
			)}

			{editing && <ScheduleEditor initial={editing} onSave={upsert} onClose={() => setEditing(null)} />}
		</div>
	);
}

function ScheduleEditor(props: { initial: Schedule; onSave: (s: Schedule) => void; onClose: () => void }) {
	const [s, setS] = useState<Schedule>(props.initial);
	const set = (patch: Partial<Schedule>) => setS((prev) => ({ ...prev, ...patch }));

	return (
		<div className="modal-overlay" onClick={props.onClose}>
			<div className="modal sched-editor" onClick={(e) => e.stopPropagation()}>
				<div className="modal-title">{props.initial.name || props.initial.prompt ? "Edit schedule" : "New schedule"}</div>

				<label>
					Name
					<input
						placeholder="e.g. Daily monthly-close check"
						value={s.name}
						onChange={(e) => set({ name: e.target.value })}
					/>
				</label>

				<label>
					Prompt
					<textarea
						placeholder="What should the agent do each run?"
						value={s.prompt}
						onChange={(e) => set({ prompt: e.target.value })}
					/>
				</label>

				<label>
					Frequency
					<select value={s.frequency} onChange={(e) => set({ frequency: e.target.value as Frequency })}>
						{FREQS.map((f) => (
							<option key={f} value={f}>
								{f[0].toUpperCase() + f.slice(1)}
							</option>
						))}
					</select>
				</label>

				<div className="sched-time-row">
					<label>
						Time
						<input type="time" value={s.time} onChange={(e) => set({ time: e.target.value })} />
					</label>
					{s.frequency === "weekly" && (
						<label>
							Day
							<select value={s.weekday} onChange={(e) => set({ weekday: Number(e.target.value) })}>
								{WEEKDAYS.map((d, i) => (
									<option key={d} value={i}>
										{d}
									</option>
								))}
							</select>
						</label>
					)}
					{s.frequency === "monthly" && (
						<label>
							Day of month
							<input
								type="number"
								min={1}
								max={31}
								value={s.dayOfMonth}
								onChange={(e) => set({ dayOfMonth: Math.min(31, Math.max(1, Number(e.target.value) || 1)) })}
							/>
						</label>
					)}
				</div>

				<label className="check-row">
					<input type="checkbox" checked={s.enabled} onChange={(e) => set({ enabled: e.target.checked })} />
					Enabled
				</label>

				<div className="modal-actions">
					<button onClick={props.onClose}>Cancel</button>
					<button className="primary" disabled={!s.prompt.trim()} onClick={() => props.onSave(s)}>
						Save schedule
					</button>
				</div>
			</div>
		</div>
	);
}
