import cron, { type ScheduledTask } from "node-cron";
import { discoverSchedules, updateScheduleMeta, type ScheduleDefinition } from "./schedules.js";
import { mkdirSync, appendFileSync } from "fs";
import { access } from "fs/promises";
import { join } from "path";
import type { ServerMessage } from "./adapter.js";
import { findBrief, resolveBriefPath } from "./brief/storage.js";
import { runBriefOrchestration } from "./brief/orchestrator.js";
import { runWithBrief } from "./brief/runner.js";

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;

export interface SchedulerOptions {
	agentDir: string;
	model?: string;
	env?: string;
	runPrompt: (prompt: string) => Promise<string>;
	broadcastToBrowsers: (msg: ServerMessage) => void;
	appendToHistory: (msg: any) => void;
}

const activeTasks = new Map<string, ScheduledTask>();
const activeTimers = new Map<string, ReturnType<typeof setTimeout>>();
const runningJobs = new Set<string>();

export async function startScheduler(opts: SchedulerOptions): Promise<void> {
	const schedules = await discoverSchedules(opts.agentDir);
	let activeCount = 0;

	for (const schedule of schedules) {
		if (!schedule.enabled) continue;

		if (schedule.mode === "once" && schedule.runAt) {
			// One-time schedule via runAt datetime
			const delay = new Date(schedule.runAt).getTime() - Date.now();
			if (delay <= 0) {
				console.log(dim(`[scheduler] "${schedule.id}" runAt is in the past — skipping`));
				continue;
			}
			const timer = setTimeout(() => {
				executeScheduledJob(schedule, opts, true);
			}, delay);
			activeTimers.set(schedule.id, timer);
			const when = new Date(schedule.runAt).toLocaleString();
			console.log(dim(`[scheduler] "${schedule.id}" scheduled once at ${when} (in ${Math.round(delay / 1000)}s)`));
			activeCount++;
		} else if (schedule.mode === "once" && schedule.cron) {
			// One-time schedule via cron — fires once then auto-disables
			if (!cron.validate(schedule.cron)) {
				console.log(dim(`[scheduler] Invalid cron for "${schedule.id}": ${schedule.cron} — skipping`));
				continue;
			}
			const task = cron.schedule(schedule.cron, () => {
				executeScheduledJob(schedule, opts, true);
			});
			activeTasks.set(schedule.id, task);
			activeCount++;
		} else {
			// Repeating cron schedule
			if (!cron.validate(schedule.cron)) {
				console.log(dim(`[scheduler] Invalid cron for "${schedule.id}": ${schedule.cron} — skipping`));
				continue;
			}
			const task = cron.schedule(schedule.cron, () => {
				executeScheduledJob(schedule, opts, false);
			});
			activeTasks.set(schedule.id, task);
			activeCount++;
		}
	}

	console.log(dim(`[scheduler] Loaded ${schedules.length} schedules (${activeCount} active)`));
}

export function stopScheduler(): void {
	for (const [, task] of activeTasks) {
		task.stop();
	}
	activeTasks.clear();
	for (const [, timer] of activeTimers) {
		clearTimeout(timer);
	}
	activeTimers.clear();
	console.log(dim("[scheduler] Stopped all scheduled tasks"));
}

export async function reloadSchedules(opts: SchedulerOptions): Promise<void> {
	stopScheduler();
	await startScheduler(opts);
}

export async function executeScheduledJob(schedule: ScheduleDefinition, opts: SchedulerOptions, disableAfterRun = false): Promise<void> {
	if (runningJobs.has(schedule.id)) {
		console.log(dim(`[scheduler] Skipping "${schedule.id}" — already running`));
		return;
	}
	runningJobs.add(schedule.id);
	const ts = new Date().toISOString();
	console.log(dim(`[scheduler] Running "${schedule.id}" at ${ts}`));

	// Broadcast schedule start to chat
	const startMsg = { type: "schedule_start", id: schedule.id, prompt: schedule.prompt, ts } as any;
	opts.broadcastToBrowsers(startMsg as ServerMessage);
	opts.appendToHistory(startMsg);

	let result = "";
	let success = true;

	// Resolve which brief file (if any) applies to this scheduled task.
	// Explicit schedule.briefPath wins; missing file triggers auto-creation.
	// Otherwise fall back to guessing by matching the task prompt.
	let briefFilePath: string | undefined;
	if (schedule.briefPath) {
		const resolved = resolveBriefPath(opts.agentDir, schedule.briefPath);
		try {
			await access(resolved);
			briefFilePath = resolved;
		} catch {
			console.log(dim(`[scheduler] ⚠ Brief "${schedule.briefPath}" not found for "${schedule.id}" — creating a new one`));
			try {
				const created = await runBriefOrchestration({
					task: schedule.prompt,
					agentDir: opts.agentDir,
					model: opts.model,
					options: { skipApproval: true },
				});
				if (!created.skipped) {
					briefFilePath = created.brief.file_path;
					console.log(dim(`[scheduler] Created brief for "${schedule.id}": ${briefFilePath}`));
				}
			} catch (err: any) {
				console.log(dim(`[scheduler] ⚠ Failed to create brief for "${schedule.id}": ${err.message} — running without a brief`));
			}
		}
	} else {
		try {
			const existing = await findBrief(opts.agentDir, schedule.prompt);
			if (existing) briefFilePath = existing.file_path;
		} catch {
			// Brief lookup failure is non-fatal
		}
	}

	try {
		if (briefFilePath) {
			console.log(dim(`[scheduler] Running "${schedule.id}" with brief: ${briefFilePath}`));
			let finalText = "";
			for await (const msg of runWithBrief({
				prompt: schedule.prompt,
				dir: opts.agentDir,
				model: opts.model,
				env: opts.env,
				brief: { briefPath: briefFilePath },
				maxRetries: 2,
			})) {
				if (msg.type === "assistant") finalText = msg.content;
			}
			result = finalText;
		} else {
			result = await opts.runPrompt(schedule.prompt);
		}
	} catch (err: any) {
		result = err.message || "Unknown error";
		success = false;
	}

	// Write to JSONL log
	try {
		const logDir = join(opts.agentDir, ".gitagent", "schedule-logs");
		mkdirSync(logDir, { recursive: true });
		const logFile = join(logDir, `${schedule.id}.jsonl`);
		const logEntry = JSON.stringify({ ts, success, result: result.slice(0, 5000) }) + "\n";
		appendFileSync(logFile, logEntry, "utf-8");
	} catch {
		// Log write failure is non-fatal
	}

	// Update schedule metadata (and auto-disable for "once" mode)
	try {
		await updateScheduleMeta(opts.agentDir, schedule.id, {
			lastRunAt: ts,
			lastResult: success ? "success" : "error",
			...(disableAfterRun ? { enabled: false } : {}),
		});
	} catch {
		// Meta update failure is non-fatal
	}

	// Stop the cron task / clear timer if this was a one-time job
	if (disableAfterRun) {
		const task = activeTasks.get(schedule.id);
		if (task) { task.stop(); activeTasks.delete(schedule.id); }
		const timer = activeTimers.get(schedule.id);
		if (timer) { clearTimeout(timer); activeTimers.delete(schedule.id); }
		console.log(dim(`[scheduler] "${schedule.id}" auto-disabled (run-once)`));
	}

	// Broadcast to connected browsers and persist to chat history
	const endMsg = {
		type: "schedule_result",
		id: schedule.id,
		result: result.slice(0, 2000),
		success,
		ts,
	} as any;
	opts.broadcastToBrowsers(endMsg as ServerMessage);
	opts.appendToHistory(endMsg);

	runningJobs.delete(schedule.id);
	console.log(dim(`[scheduler] "${schedule.id}" completed (${success ? "success" : "error"})`));
}
