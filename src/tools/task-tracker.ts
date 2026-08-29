import { readFile, writeFile, mkdir, readdir } from "fs/promises";
import { join } from "path";
import { randomUUID } from "crypto";
import { type Static } from "@sinclair/typebox";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { Model } from "@mariozechner/pi-ai";
import type { GCAssistantMessage } from "../sdk-types.js";
import { taskTrackerSchema } from "./shared.js";
import { adjustConfidence, loadSkillStats, saveSkillStats } from "../learning/reinforcement.js";
import { reflectOnFailure } from "../learning/reflection.js";
import type { Elicitor } from "../elicit.js";
import yaml from "js-yaml";

// ── Types ───────────────────────────────────────────────────────────────

interface TaskStep {
	description: string;
	timestamp: string;
}

export interface TaskRecord {
	id: string;
	objective: string;
	steps: TaskStep[];
	attempts: number;
	status: "active" | "succeeded" | "failed";
	outcome?: "success" | "failure" | "partial";
	failure_reason?: string;
	skill_used?: string;
	started_at: string;
	ended_at?: string;
}

interface TasksStore {
	tasks: TaskRecord[];
}

// ── Persistence ─────────────────────────────────────────────────────────

async function loadTasks(gitagentDir: string): Promise<TasksStore> {
	const tasksFile = join(gitagentDir, "learning", "tasks.json");
	try {
		const raw = await readFile(tasksFile, "utf-8");
		return JSON.parse(raw) as TasksStore;
	} catch {
		return { tasks: [] };
	}
}

async function saveTasks(gitagentDir: string, store: TasksStore): Promise<void> {
	const learningDir = join(gitagentDir, "learning");
	await mkdir(learningDir, { recursive: true });
	await writeFile(join(learningDir, "tasks.json"), JSON.stringify(store, null, 2), "utf-8");
}

// ── Skill search ────────────────────────────────────────────────────────

function extractKeywords(text: string): string[] {
	return text
		.toLowerCase()
		.replace(/[^a-z0-9\s-]/g, "")
		.split(/\s+/)
		.filter((w) => w.length > 2);
}

function keywordOverlap(a: string[], b: string[]): number {
	const setB = new Set(b);
	const matches = a.filter((w) => setB.has(w)).length;
	if (a.length === 0 || b.length === 0) return 0;
	return matches / Math.max(a.length, b.length);
}

interface SkillMatch {
	name: string;
	description: string;
	confidence?: number;
	source: "local" | "marketplace";
	relevance: number;
	/** Local skills only — shown to the user when a flagged match needs a decision. */
	successCount?: number;
	failureCount?: number;
	negativeExamples?: string[];
}

async function searchLocalSkills(agentDir: string, objective: string): Promise<SkillMatch[]> {
	const skillsDir = join(agentDir, "skills");
	const objKeywords = extractKeywords(objective);
	const matches: SkillMatch[] = [];

	let entries;
	try {
		entries = await readdir(skillsDir, { withFileTypes: true });
	} catch {
		return [];
	}

	for (const entry of entries) {
		if (!entry.isDirectory()) continue;

		const skillFile = join(skillsDir, entry.name, "SKILL.md");
		let content: string;
		try {
			content = await readFile(skillFile, "utf-8");
		} catch {
			continue;
		}

		const fmMatch = content.match(/^---\r?\n([\s\S]*?)\r?\n---/);
		if (!fmMatch) continue;

		const frontmatter = yaml.load(fmMatch[1]) as Record<string, any>;
		const name = frontmatter.name as string;
		const description = (frontmatter.description as string) || "";

		if (!name) continue;

		const skillKeywords = extractKeywords(`${name} ${description}`);
		const relevance = keywordOverlap(objKeywords, skillKeywords);

		if (relevance > 0.1) {
			matches.push({
				name,
				description,
				confidence: typeof frontmatter.confidence === "number" ? frontmatter.confidence : undefined,
				source: "local",
				relevance: Math.round(relevance * 100) / 100,
				successCount: typeof frontmatter.success_count === "number" ? frontmatter.success_count : undefined,
				failureCount: typeof frontmatter.failure_count === "number" ? frontmatter.failure_count : undefined,
				negativeExamples: Array.isArray(frontmatter.negative_examples)
					? (frontmatter.negative_examples as string[])
					: undefined,
			});
		}
	}

	return matches.sort((a, b) => b.relevance - a.relevance);
}

async function searchSkillsMP(objective: string): Promise<SkillMatch[]> {
	const apiKey = process.env.SKILLSMP_API_KEY;
	if (!apiKey) return [];

	try {
		const url = `https://api.skillsmp.com/v1/search?q=${encodeURIComponent(objective)}`;
		const resp = await fetch(url, {
			headers: { Authorization: `Bearer ${apiKey}` },
			signal: AbortSignal.timeout(5000),
		});
		if (!resp.ok) return [];

		const data = (await resp.json()) as { results?: Array<{ name: string; description: string; relevance: number }> };
		return (data.results || []).map((r) => ({
			name: r.name,
			description: r.description,
			source: "marketplace" as const,
			relevance: r.relevance,
		}));
	} catch {
		return [];
	}
}

// ── Tool factory ────────────────────────────────────────────────────────

/** Renders the evidence a human needs to decide whether a flagged skill is worth using. */
function describeFlaggedSkill(match: SkillMatch): string {
	const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
	const lines = [`  ${match.name} — ${match.description}`];

	const stats: string[] = [];
	if (match.confidence !== undefined) stats.push(`confidence ${match.confidence}`);
	if (match.successCount !== undefined && match.failureCount !== undefined) {
		stats.push(`${match.successCount} success / ${match.failureCount} failure`);
	}
	if (stats.length > 0) lines.push(dim(`  ${stats.join(" · ")}`));

	const recent = (match.negativeExamples ?? []).slice(-3);
	if (recent.length > 0) {
		lines.push(dim("  recent failures:"));
		for (const n of recent) {
			const short = n.length > 160 ? `${n.slice(0, 160)}…` : n;
			lines.push(dim(`    - ${short}`));
		}
	}

	return lines.join("\n");
}

export function createTaskTrackerTool(
	agentDir: string,
	gitagentDir: string,
	model?: Model<any>,
	onUsage?: (msg: GCAssistantMessage) => void,
	elicit?: Elicitor,
	autoRepair?: boolean,
): AgentTool<typeof taskTrackerSchema> {
	return {
		name: "task_tracker",
		label: "task_tracker",
		description:
			"Track multi-step tasks for outcome-driven learning. Use 'begin' to start tracking (auto-searches for matching skills), 'update' to log steps, 'end' to report success/failure (triggers reinforcement learning), 'list' to see active tasks.",
		parameters: taskTrackerSchema,
		execute: async (
			_toolCallId: string,
			rawParams: unknown,
			signal?: AbortSignal,
		) => {
			const params = rawParams as Static<typeof taskTrackerSchema>;
			if (signal?.aborted) throw new Error("Operation aborted");

			const store = await loadTasks(gitagentDir);

			switch (params.action) {
				case "begin": {
					if (!params.objective) {
						throw new Error("objective is required for begin action");
					}

					// Check for existing active tasks with same objective (retry)
					const existing = store.tasks.find(
						(t) => t.status === "active" && t.objective === params.objective,
					);
					if (existing) {
						existing.attempts++;
						await saveTasks(gitagentDir, store);
						return {
							content: [{
								type: "text",
								text: `Resuming task ${existing.id} (attempt #${existing.attempts})\nObjective: ${existing.objective}`,
							}],
							details: { task_id: existing.id, attempts: existing.attempts },
						};
					}

					// Check for prior failed attempts with same objective
					const priorFailed = store.tasks.filter(
						(t) => t.status === "failed" && t.objective === params.objective,
					);

					// Search for matching skills
					const [localMatches, mpMatches] = await Promise.all([
						searchLocalSkills(agentDir, params.objective),
						searchSkillsMP(params.objective),
					]);
					const allMatches = [...localMatches, ...mpMatches];

					// Create new task
					const task: TaskRecord = {
						id: randomUUID(),
						objective: params.objective,
						steps: [],
						attempts: priorFailed.length + 1,
						status: "active",
						started_at: new Date().toISOString(),
					};
					store.tasks.push(task);
					await saveTasks(gitagentDir, store);

					let response = `Task started: ${task.id}\nObjective: ${task.objective}`;
					if (task.attempts > 1) {
						response += `\nAttempt #${task.attempts}`;
						const reasons = priorFailed
							.filter((t) => t.failure_reason)
							.map((t) => `- ${t.failure_reason}`)
							.join("\n");
						if (reasons) {
							response += `\n\nPrior failures:\n${reasons}\n\nAvoid these approaches.`;
						}
					}

					// "p" | "r" | "s" once a human has ruled on a flagged match; null otherwise.
					let flaggedDecision: string | null = null;

					if (allMatches.length > 0) {
						const topMatch = allMatches[0];
						const topConf = topMatch.confidence !== undefined ? ` (confidence: ${topMatch.confidence})` : "";
						// Matches isSkillFlagged()'s threshold in reinforcement.ts — don't push
						// a known-unreliable skill as mandatory without surfacing that first.
						const topFlagged = topMatch.confidence !== undefined && topMatch.confidence < 0.4;

						if (topFlagged) {
							// A flagged skill is a judgement call, not a mechanical one: ask the
							// human which way to go and hand the model their decision, instead of
							// letting it pick repair-vs-proceed on its own. With no terminal, the
							// autoRepair flag decides whether repair is even on the table.
							const decision = flaggedDecision = elicit?.interactive
								? await elicit.select({
									title: `⚠️  Skill "${topMatch.name}" matches this task but is flagged as unreliable`,
									body: describeFlaggedSkill(topMatch),
									choices: [
										{ key: "p", label: "proceed — use the skill as-is" },
										{ key: "r", label: "repair — rewrite its steps first (you review the change)" },
										{ key: "s", label: "skip — ignore the skill, solve from scratch" },
									],
									defaultKey: "p",
									signal,
								})
								: null;

							response += `\n\n⚠️ SKILL MATCH FOUND, BUT IT'S FLAGGED AS UNRELIABLE:`;
							response += `\n  → ${topMatch.name}: ${topMatch.description}${topConf} [${topMatch.source}]`;

							if (decision === "p") {
								response += `\n\nUSER DECISION: proceed with the flagged skill as-is.`;
								response += `\nACTION REQUIRED: Load skills/${topMatch.name}/SKILL.md NOW and follow its instructions.`;
								response += `\nDo NOT call skill_learner action "repair" — the user declined that. Report the real outcome via task_tracker "end" with skill_used "${topMatch.name}".`;
							} else if (decision === "r") {
								response += `\n\nUSER DECISION: repair the skill before using it.`;
								response += `\nACTION REQUIRED: Call skill_learner action "repair" with skill_name "${topMatch.name}" NOW. The user reviews and approves the rewritten steps.`;
								response += `\nIf the repair is approved, load skills/${topMatch.name}/SKILL.md and follow the repaired steps. If the user cancels the repair, do not use the skill — solve the task from scratch.`;
							} else if (decision === "s") {
								response += `\n\nUSER DECISION: skip the skill entirely.`;
								response += `\nACTION REQUIRED: Do NOT read or use skills/${topMatch.name}/SKILL.md, and do NOT call skill_learner "repair". Solve this task from scratch and do not pass skill_used to task_tracker "end".`;
							} else if (autoRepair) {
								response += `\n\nThis skill has a low confidence score from repeated failures, and automatic repair is ENABLED for this run.`;
								response += `\nACTION REQUIRED: Call skill_learner action "repair" with skill_name "${topMatch.name}" first, then load skills/${topMatch.name}/SKILL.md and follow the repaired steps.`;
							} else {
								response += `\n\nThis skill has a low confidence score from repeated failures, and repair is DISABLED for this run.`;
								response += `\nDo NOT call skill_learner action "repair" — it will be refused. Either load skills/${topMatch.name}/SKILL.md and use it with caution (reporting the real outcome via task_tracker "end"), or solve the task from scratch.`;
							}
						} else {
							response += `\n\n⚡ SKILL MATCH FOUND — YOU MUST USE IT:`;
							response += `\n  → ${topMatch.name}: ${topMatch.description}${topConf} [${topMatch.source}]`;
							response += `\n\nACTION REQUIRED: Load skills/${topMatch.name}/SKILL.md NOW and follow its instructions.`;
							response += `\nDo NOT proceed with a manual approach — the skill handles this task.`;
						}
						if (allMatches.length > 1) {
							response += `\n\nOther matching skills:`;
							for (const m of allMatches.slice(1, 5)) {
								const conf = m.confidence !== undefined ? ` (confidence: ${m.confidence})` : "";
								response += `\n  - ${m.name}: ${m.description}${conf} [${m.source}]`;
							}
						}
					} else {
						response += "\n\nNo matching skills found. Solve from scratch.";
					}

					return {
						content: [{ type: "text", text: response }],
						details: { task_id: task.id, matches: allMatches, flagged_decision: flaggedDecision ?? undefined },
					};
				}

				case "update": {
					if (!params.task_id) throw new Error("task_id is required for update action");
					if (!params.step) throw new Error("step is required for update action");

					const task = store.tasks.find((t) => t.id === params.task_id);
					if (!task) throw new Error(`Task not found: ${params.task_id}`);
					if (task.status !== "active") throw new Error(`Task ${params.task_id} is not active (status: ${task.status})`);

					task.steps.push({
						description: params.step,
						timestamp: new Date().toISOString(),
					});
					await saveTasks(gitagentDir, store);

					return {
						content: [{ type: "text", text: `Step ${task.steps.length} recorded: ${params.step}` }],
						details: { step_number: task.steps.length },
					};
				}

				case "end": {
					if (!params.task_id) throw new Error("task_id is required for end action");
					if (!params.outcome) throw new Error("outcome is required for end action");

					const task = store.tasks.find((t) => t.id === params.task_id);
					if (!task) throw new Error(`Task not found: ${params.task_id}`);
					if (task.status !== "active") throw new Error(`Task ${params.task_id} is not active (status: ${task.status})`);

					const outcome = params.outcome as "success" | "failure" | "partial";

					// Reflexion-style reflection: on any non-success outcome, replace the
					// model's own one-line failure report with a grounded root-cause +
					// next-strategy reflection generated from the task's actual recorded
					// steps. Fails soft — any error here keeps the raw string exactly as
					// before this reflection step existed.
					let effectiveFailureReason = params.failure_reason;
					if (outcome !== "success" && model) {
						try {
							effectiveFailureReason = await reflectOnFailure(
								model,
								{
									objective: task.objective,
									steps: task.steps.map((s) => s.description),
									failureReason: params.failure_reason,
								},
								onUsage,
							);
						} catch {
							// fail-soft — keep the raw one-liner
						}
					}

					task.outcome = outcome;
					task.status = outcome === "success" ? "succeeded" : "failed";
					task.ended_at = new Date().toISOString();
					task.failure_reason = effectiveFailureReason;
					task.skill_used = params.skill_used;

					// Trigger reinforcement if a skill was used
					let reinforcementMsg = "";
					if (params.skill_used) {
						const skillDir = join(agentDir, "skills", params.skill_used);
						try {
							const stats = await loadSkillStats(skillDir);
							const updated = adjustConfidence(stats, outcome, effectiveFailureReason);
							await saveSkillStats(skillDir, updated);
							reinforcementMsg = `\nSkill "${params.skill_used}" confidence: ${stats.confidence} → ${updated.confidence}`;
						} catch {
							reinforcementMsg = `\nCould not update skill "${params.skill_used}" stats (skill may not exist).`;
						}
					}

					await saveTasks(gitagentDir, store);

					if (outcome === "success") {
						return {
							content: [{
								type: "text",
								text: `Task ${task.id} completed successfully (${task.steps.length} steps).${reinforcementMsg}\n\nConsider calling skill_learner action "evaluate" with this task_id to check if this approach is worth saving as a reusable skill.`,
							}],
							details: { task_id: task.id },
						};
					}

					return {
						content: [{
							type: "text",
							text: `Task ${task.id} ${outcome}. Reason: ${effectiveFailureReason || "not specified"}.${reinforcementMsg}\n\nConsider a different approach. Call task_tracker action "begin" with the same objective to retry.`,
						}],
						details: { task_id: task.id },
					};
				}

				case "list": {
					const active = store.tasks.filter((t) => t.status === "active");
					if (active.length === 0) {
						return {
							content: [{ type: "text", text: "No active tasks." }],
							details: undefined,
						};
					}

					const lines = active.map((t) =>
						`- ${t.id}: "${t.objective}" (${t.steps.length} steps, attempt #${t.attempts})`,
					);
					return {
						content: [{ type: "text", text: `Active tasks:\n${lines.join("\n")}` }],
						details: { count: active.length },
					};
				}

				default:
					throw new Error(`Unknown action: ${params.action}`);
			}
		},
	};
}
