import { readFile, readdir, stat, writeFile, unlink } from "fs/promises";
import { join } from "path";
import { mkdirSync } from "fs";
import yaml from "js-yaml";

export interface ScheduleDefinition {
	id: string;
	prompt: string;
	cron: string;
	mode: "repeat" | "once";
	runAt?: string; // ISO datetime for "once" mode (alternative to cron)
	enabled: boolean;
	createdAt: string;
	lastRunAt?: string;
	lastResult?: string;
}

const KEBAB_RE = /^[a-z0-9]+(-[a-z0-9]+)*$/;

export async function discoverSchedules(agentDir: string): Promise<ScheduleDefinition[]> {
	const schedulesDir = join(agentDir, "schedules");

	try {
		const s = await stat(schedulesDir);
		if (!s.isDirectory()) return [];
	} catch {
		return [];
	}

	const entries = await readdir(schedulesDir);
	const schedules: ScheduleDefinition[] = [];

	for (const entry of entries) {
		if (!entry.endsWith(".yaml") && !entry.endsWith(".yml")) continue;
		const filePath = join(schedulesDir, entry);
		const s = await stat(filePath);
		if (!s.isFile()) continue;

		try {
			const raw = await readFile(filePath, "utf-8");
			const data = yaml.load(raw) as Record<string, any>;
			if (data?.id && KEBAB_RE.test(String(data.id)) && data?.prompt && (data?.cron || data?.runAt)) {
				schedules.push({
					id: String(data.id),
					prompt: String(data.prompt),
					cron: String(data.cron || ""),
					mode: data.mode === "once" ? "once" : "repeat",
					...(data.runAt ? { runAt: String(data.runAt) } : {}),
					enabled: data.enabled !== false,
					createdAt: String(data.createdAt || new Date().toISOString()),
					...(data.lastRunAt ? { lastRunAt: String(data.lastRunAt) } : {}),
					...(data.lastResult ? { lastResult: String(data.lastResult) } : {}),
				});
			}
		} catch {
			// Skip invalid YAML
		}
	}

	return schedules.sort((a, b) => a.id.localeCompare(b.id));
}

export async function loadSchedule(filePath: string): Promise<ScheduleDefinition> {
	const raw = await readFile(filePath, "utf-8");
	const data = yaml.load(raw) as Record<string, any>;
	if (!data?.id || !data?.prompt || (!data?.cron && !data?.runAt)) {
		throw new Error("Invalid schedule definition: missing id, prompt, or cron/runAt");
	}
	if (!KEBAB_RE.test(String(data.id))) {
		throw new Error("Schedule id must be kebab-case (e.g. daily-standup)");
	}
	return {
		id: String(data.id),
		prompt: String(data.prompt),
		cron: String(data.cron || ""),
		mode: data.mode === "once" ? "once" : "repeat",
		...(data.runAt ? { runAt: String(data.runAt) } : {}),
		enabled: data.enabled !== false,
		createdAt: String(data.createdAt || new Date().toISOString()),
		...(data.lastRunAt ? { lastRunAt: String(data.lastRunAt) } : {}),
		...(data.lastResult ? { lastResult: String(data.lastResult) } : {}),
	};
}

export async function saveSchedule(agentDir: string, schedule: ScheduleDefinition): Promise<string> {
	if (!KEBAB_RE.test(schedule.id)) {
		throw new Error("Schedule id must be kebab-case (e.g. daily-standup)");
	}
	if (!schedule.prompt || (!schedule.cron && !schedule.runAt)) {
		throw new Error("Schedule must have a prompt and cron expression or runAt time");
	}
	const schedulesDir = join(agentDir, "schedules");
	mkdirSync(schedulesDir, { recursive: true });
	const filePath = join(schedulesDir, `${schedule.id}.yaml`);
	const content = yaml.dump({
		id: schedule.id,
		prompt: schedule.prompt,
		cron: schedule.cron || "",
		mode: schedule.mode || "repeat",
		...(schedule.runAt ? { runAt: schedule.runAt } : {}),
		enabled: schedule.enabled,
		createdAt: schedule.createdAt || new Date().toISOString(),
		...(schedule.lastRunAt ? { lastRunAt: schedule.lastRunAt } : {}),
		...(schedule.lastResult ? { lastResult: schedule.lastResult } : {}),
	}, { lineWidth: 120 });
	await writeFile(filePath, content, "utf-8");
	return filePath;
}

export async function deleteSchedule(agentDir: string, id: string): Promise<void> {
	if (!KEBAB_RE.test(id)) {
		throw new Error("Schedule id must be kebab-case (e.g. daily-standup)");
	}
	const filePath = join(agentDir, "schedules", `${id}.yaml`);
	await unlink(filePath);
}

export async function updateScheduleMeta(agentDir: string, id: string, updates: Partial<Pick<ScheduleDefinition, "lastRunAt" | "lastResult" | "enabled">>): Promise<void> {
	if (!KEBAB_RE.test(id)) {
		throw new Error("Schedule id must be kebab-case (e.g. daily-standup)");
	}
	const filePath = join(agentDir, "schedules", `${id}.yaml`);
	const raw = await readFile(filePath, "utf-8");
	const data = yaml.load(raw) as Record<string, any>;
	Object.assign(data, updates);
	const content = yaml.dump(data, { lineWidth: 120 });
	await writeFile(filePath, content, "utf-8");
}
