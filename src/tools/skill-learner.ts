import { readFile, writeFile, mkdir, readdir, rm } from "fs/promises";
import { join } from "path";
import { execSync } from "child_process";
import { type Static } from "@sinclair/typebox";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { Model } from "@mariozechner/pi-ai";
import type { GCAssistantMessage } from "../sdk-types.js";
import { skillLearnerSchema } from "./shared.js";
import { loadSkillStats, isSkillFlagged } from "../learning/reinforcement.js";
import { repairSkillSteps } from "../learning/skill-repair.js";
import type { TaskRecord } from "./task-tracker.js";
import type { Elicitor } from "../elicit.js";
import { renderDiff } from "../text-diff.js";
import yaml from "js-yaml";

// Caps how many times a single skill can be auto-repaired before it must
// go to a human (via "update" or "delete") instead.
const MAX_REPAIRS = 3;
// Clears the <0.4 flag immediately but stays below what a genuinely-proven
// skill earns — a repaired skill has to re-earn trust, not start clean.
const REPAIR_RESET_CONFIDENCE = 0.6;

// ── Helpers ─────────────────────────────────────────────────────────────

interface TasksStore {
	tasks: TaskRecord[];
}

async function loadTasks(gitagentDir: string): Promise<TasksStore> {
	const tasksFile = join(gitagentDir, "learning", "tasks.json");
	try {
		const raw = await readFile(tasksFile, "utf-8");
		return JSON.parse(raw) as TasksStore;
	} catch {
		return { tasks: [] };
	}
}

function extractKeywords(text: string): string[] {
	return text
		.toLowerCase()
		.replace(/[^a-z0-9\s-]/g, "")
		.split(/\s+/)
		.filter((w) => w.length > 2);
}

function jaccardSimilarity(a: string[], b: string[]): number {
	const setA = new Set(a);
	const setB = new Set(b);
	const intersection = [...setA].filter((x) => setB.has(x)).length;
	const union = new Set([...setA, ...setB]).size;
	return union === 0 ? 0 : intersection / union;
}

// Checks if a step looks project-specific (absolute paths, UUIDs, etc.)
function isProjectSpecific(step: string): boolean {
	const patterns = [
		/\/[a-zA-Z][\w/.-]{5,}/, // absolute-ish paths
		/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i, // UUID
		/[A-Z][a-z]+(?:[A-Z][a-z]+){2,}/, // PascalCase with 3+ parts (likely project-specific class)
	];
	return patterns.some((p) => p.test(step));
}

async function getExistingSkillDescriptions(agentDir: string): Promise<Array<{ name: string; keywords: string[] }>> {
	const skillsDir = join(agentDir, "skills");
	const result: Array<{ name: string; keywords: string[] }> = [];

	let entries;
	try {
		entries = await readdir(skillsDir, { withFileTypes: true });
	} catch {
		return [];
	}

	for (const entry of entries) {
		if (!entry.isDirectory()) continue;
		const skillFile = join(skillsDir, entry.name, "SKILL.md");
		try {
			const content = await readFile(skillFile, "utf-8");
			const fmMatch = content.match(/^---\r?\n([\s\S]*?)\r?\n---/);
			if (!fmMatch) continue;
			const fm = yaml.load(fmMatch[1]) as Record<string, any>;
			if (fm.description) {
				result.push({
					name: fm.name as string,
					keywords: extractKeywords(fm.description as string),
				});
			}
		} catch {
			continue;
		}
	}

	return result;
}

async function loadRepairCount(skillDir: string): Promise<number> {
	try {
		const content = await readFile(join(skillDir, "SKILL.md"), "utf-8");
		const fmMatch = content.match(/^---\r?\n([\s\S]*?)\r?\n---/);
		if (!fmMatch) return 0;
		const fm = yaml.load(fmMatch[1]) as Record<string, any>;
		return typeof fm.repair_count === "number" ? fm.repair_count : 0;
	} catch {
		return 0;
	}
}

function gitCommit(agentDir: string, files: string[], message: string): void {
	try {
		for (const f of files) {
			execSync(`git add "${f}"`, { cwd: agentDir, stdio: "pipe" });
		}
		execSync(`git commit -m "${message.replace(/"/g, '\\"')}"`, {
			cwd: agentDir,
			stdio: "pipe",
		});
	} catch {
		// Not fatal — file was still written
	}
}

// ── Tool factory ────────────────────────────────────────────────────────

/** Diff + failure lessons block shown above the accept/edit/cancel choices. */
function renderRepairPreview(
	currentSteps: string,
	proposedSteps: string,
	negativeExamples: string[],
): string {
	const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
	const parts = [
		renderDiff(currentSteps, proposedSteps, {
			beforeLabel: "current steps",
			afterLabel: "proposed steps",
		}),
	];
	if (negativeExamples.length > 0) {
		const lessons = negativeExamples
			.slice(-3)
			.map((n) => `    - ${n.length > 160 ? `${n.slice(0, 160)}…` : n}`);
		parts.push(dim(`\n  based on ${negativeExamples.length} recorded failure(s):\n${lessons.join("\n")}`));
	}
	return parts.join("\n");
}

export function createSkillLearnerTool(
	agentDir: string,
	gitagentDir: string,
	model?: Model<any>,
	onUsage?: (msg: GCAssistantMessage) => void,
	elicit?: Elicitor,
	autoRepair?: boolean,
): AgentTool<typeof skillLearnerSchema> {
	return {
		name: "skill_learner",
		label: "skill_learner",
		description:
			"Learn from successful tasks. Use 'evaluate' to check if a completed task is worth saving as a skill, 'crystallize' to save it, 'status' to list all skills with confidence scores, 'review' to see flagged low-confidence skills, 'repair' to have the agent rewrite a flagged skill's steps using its own accumulated failure lessons, 'update' to modify a skill, 'delete' to remove one.",
		parameters: skillLearnerSchema,
		execute: async (
			_toolCallId: string,
			rawParams: unknown,
			signal?: AbortSignal,
		) => {
			const params = rawParams as Static<typeof skillLearnerSchema>;
			if (signal?.aborted) throw new Error("Operation aborted");

			switch (params.action) {
				case "evaluate": {
					if (!params.task_id) throw new Error("task_id is required for evaluate action");

					const store = await loadTasks(gitagentDir);
					const task = store.tasks.find((t) => t.id === params.task_id);
					if (!task) throw new Error(`Task not found: ${params.task_id}`);
					if (task.status !== "succeeded") {
						return {
							content: [{ type: "text", text: `Task ${params.task_id} did not succeed (status: ${task.status}). Only successful tasks can become skills.` }],
							details: undefined,
						};
					}

					// Skill-worthiness heuristic
					const checks = {
						multi_step: task.steps.length >= 3,
						non_trivial: task.steps.length >= 2,
						novel: true,
						generalizable: true,
					};

					// Check novelty: no existing skill with >0.5 Jaccard similarity
					const taskKeywords = extractKeywords(task.objective);
					const existingSkills = await getExistingSkillDescriptions(agentDir);
					for (const skill of existingSkills) {
						if (jaccardSimilarity(taskKeywords, skill.keywords) > 0.5) {
							checks.novel = false;
							break;
						}
					}

					// Check generalizability: <30% of steps are project-specific
					const specificSteps = task.steps.filter((s) => isProjectSpecific(s.description)).length;
					checks.generalizable = specificSteps / Math.max(task.steps.length, 1) < 0.3;

					const passCount = Object.values(checks).filter(Boolean).length;
					const worthy = params.override_heuristic || passCount >= 3 || (checks.multi_step && checks.novel);

					const reasons = [
						`Multi-step (${task.steps.length} steps): ${checks.multi_step ? "PASS" : "FAIL"}`,
						`Non-trivial: ${checks.non_trivial ? "PASS" : "FAIL"}`,
						`Novel: ${checks.novel ? "PASS" : "FAIL"}`,
						`Generalizable: ${checks.generalizable ? "PASS" : "FAIL"}`,
					];

					if (worthy) {
						return {
							content: [{
								type: "text",
								text: `Task IS worthy of becoming a skill.\n\nChecks:\n${reasons.join("\n")}\n\nCall skill_learner action "crystallize" with this task_id, a skill_name (kebab-case), and a skill_description.`,
							}],
							details: { worthy: true, checks },
						};
					}

					return {
						content: [{
							type: "text",
							text: `Task is NOT worthy of becoming a skill (${passCount}/4 checks passed).\n\nChecks:\n${reasons.join("\n")}`,
						}],
						details: { worthy: false, checks },
					};
				}

				case "crystallize": {
					if (!params.task_id) throw new Error("task_id is required for crystallize action");
					if (!params.skill_name) throw new Error("skill_name is required for crystallize action");
					if (!params.skill_description) throw new Error("skill_description is required for crystallize action");

					// Validate kebab-case
					if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(params.skill_name)) {
						throw new Error("skill_name must be kebab-case (e.g., deploy-staging)");
					}

					const store = await loadTasks(gitagentDir);
					const task = store.tasks.find((t) => t.id === params.task_id);
					if (!task) throw new Error(`Task not found: ${params.task_id}`);

					// SUCCESS GATE
					if (task.outcome !== "success") {
						throw new Error(`Cannot crystallize failed task. Only successful tasks can become skills.`);
					}

					// Build SKILL.md
					const frontmatter: Record<string, any> = {
						name: params.skill_name,
						description: params.skill_description,
						learned_from: `task:${task.id}`,
						learned_at: new Date().toISOString(),
						confidence: 1.0,
						usage_count: 0,
						success_count: 0,
						failure_count: 0,
						negative_examples: [],
					};

					const stepsSection = task.steps
						.map((s, i) => `${i + 1}. ${s.description}`)
						.join("\n");

					// Collect negative examples from prior failed attempts with same objective
					const priorFailed = store.tasks.filter(
						(t) => t.status === "failed" && t.objective === task.objective,
					);

					let whatDidNotWork = "";
					if (priorFailed.length > 0) {
						const failureReasons = priorFailed
							.filter((t) => t.failure_reason)
							.map((t) => `- ${t.failure_reason}`)
							.join("\n");
						if (failureReasons) {
							whatDidNotWork = failureReasons;
						}
					}

					let body = `\n## Steps\n${stepsSection}\n\n## What Worked\nThis approach succeeded on attempt #${task.attempts}.\n`;
					if (whatDidNotWork) {
						body += `\n## What Did NOT Work\n${whatDidNotWork}\n`;
					}

					const content = `---\n${yaml.dump(frontmatter, { lineWidth: -1, noRefs: true }).trimEnd()}\n---\n${body}`;

					// Write skill
					const skillDir = join(agentDir, "skills", params.skill_name);
					await mkdir(skillDir, { recursive: true });
					const skillFile = join(skillDir, "SKILL.md");
					await writeFile(skillFile, content, "utf-8");

					// Git commit
					gitCommit(agentDir, [`skills/${params.skill_name}/SKILL.md`], `Learn skill: ${params.skill_name}`);

					return {
						content: [{
							type: "text",
							text: `Skill "${params.skill_name}" crystallized and committed.\nPath: skills/${params.skill_name}/SKILL.md\nConfidence: 1.0\n\nThe skill is now available via /skill:${params.skill_name}`,
						}],
						details: { skill_name: params.skill_name, path: skillFile },
					};
				}

				case "status": {
					const skillsDir = join(agentDir, "skills");
					let entries;
					try {
						entries = await readdir(skillsDir, { withFileTypes: true });
					} catch {
						return {
							content: [{ type: "text", text: "No skills directory found." }],
							details: undefined,
						};
					}

					const skills: Array<{ name: string; confidence: number; usage: number; ratio: string; repairs: number; flagged: boolean }> = [];

					for (const entry of entries) {
						if (!entry.isDirectory()) continue;
						const dir = join(skillsDir, entry.name);
						const stats = await loadSkillStats(dir);
						const repairs = await loadRepairCount(dir);
						// Only include learned skills (those with stats fields)
						skills.push({
							name: entry.name,
							confidence: stats.confidence,
							usage: stats.usage_count,
							ratio: `${stats.success_count}/${stats.success_count + stats.failure_count}`,
							repairs,
							flagged: isSkillFlagged(stats),
						});
					}

					if (skills.length === 0) {
						return {
							content: [{ type: "text", text: "No skills found." }],
							details: undefined,
						};
					}

					const lines = skills.map((s) =>
						`  ${s.name}: confidence=${s.confidence}, usage=${s.usage}, success_ratio=${s.ratio}` +
						(s.repairs > 0 ? `, repairs=${s.repairs}/${MAX_REPAIRS}` : "") +
						(s.flagged ? ` ⚠️ FLAGGED — unreliable, consider action "repair"` : ""),
					);
					const flaggedCount = skills.filter((s) => s.flagged).length;
					const footer = flaggedCount > 0
						? `\n\n${flaggedCount} skill(s) flagged as unreliable. Before using one, call skill_learner action "repair" on it first, or proceed with caution.`
						: "";
					return {
						content: [{ type: "text", text: `Skills:\n${lines.join("\n")}${footer}` }],
						details: { skills },
					};
				}

				case "review": {
					const skillsDir = join(agentDir, "skills");
					let entries;
					try {
						entries = await readdir(skillsDir, { withFileTypes: true });
					} catch {
						return {
							content: [{ type: "text", text: "No skills directory found." }],
							details: undefined,
						};
					}

					const flagged: Array<{ name: string; confidence: number; negatives: string[] }> = [];

					for (const entry of entries) {
						if (!entry.isDirectory()) continue;
						const dir = join(skillsDir, entry.name);
						const stats = await loadSkillStats(dir);
						if (isSkillFlagged(stats)) {
							flagged.push({
								name: entry.name,
								confidence: stats.confidence,
								negatives: stats.negative_examples,
							});
						}
					}

					if (flagged.length === 0) {
						return {
							content: [{ type: "text", text: "No flagged skills (all confidence >= 0.4)." }],
							details: undefined,
						};
					}

					const lines = flagged.map((s) => {
						let line = `  ${s.name}: confidence=${s.confidence}`;
						if (s.negatives.length > 0) {
							line += `\n    Failures: ${s.negatives.join("; ")}`;
						}
						return line;
					});

					return {
						content: [{
							type: "text",
							text: `Flagged skills (confidence < 0.4):\n${lines.join("\n")}\n\nCall skill_learner action "repair" with a skill_name to let the agent rewrite its steps using these lessons (up to ${MAX_REPAIRS} times per skill). If a skill has already been repaired ${MAX_REPAIRS} times, use "update" or "delete" instead.`,
						}],
						details: { flagged },
					};
				}

				case "repair": {
					if (!params.skill_name) throw new Error("skill_name is required for repair action");
					if (!model) throw new Error("Repair requires a model to be configured for this agent.");

					const skillFile = join(agentDir, "skills", params.skill_name, "SKILL.md");
					let content: string;
					try {
						content = await readFile(skillFile, "utf-8");
					} catch {
						throw new Error(`Skill not found: ${params.skill_name}`);
					}

					const fmMatch = content.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
					if (!fmMatch) throw new Error("Invalid SKILL.md format");
					const frontmatter = yaml.load(fmMatch[1]) as Record<string, any>;
					const body = fmMatch[2];

					const skillDir = join(agentDir, "skills", params.skill_name);
					const stats = await loadSkillStats(skillDir);
					if (!isSkillFlagged(stats)) {
						throw new Error(
							`Skill "${params.skill_name}" is not flagged (confidence ${stats.confidence} >= 0.4). Repair is only for flagged skills.`,
						);
					}

					const repairCount = typeof frontmatter.repair_count === "number" ? frontmatter.repair_count : 0;
					if (repairCount >= MAX_REPAIRS) {
						throw new Error(
							`Skill "${params.skill_name}" has already been repaired ${repairCount}/${MAX_REPAIRS} times. Use "update" or "delete" instead.`,
						);
					}

					// A repair rewrites and commits a file the agent will keep following, so
					// it needs someone to sign off: a human at the terminal, or an explicit
					// autoRepair opt-in. With neither, refuse before spending an LLM call.
					if (!elicit?.interactive && !autoRepair) {
						return {
							content: [{
								type: "text",
								text: `Repair of "${params.skill_name}" was NOT applied — repair needs approval, and this run has neither an interactive terminal nor autoRepair enabled. ` +
									`skills/${params.skill_name}/SKILL.md is unchanged (confidence still ${stats.confidence}).\n\n` +
									`Do NOT retry. Either use the skill as-is and report the real outcome, or solve the task from scratch.`,
							}],
							details: { skill_name: params.skill_name, approved: false, reason: "no_approval_channel" },
						};
					}

					const stepsMatch = body.match(/## Steps\r?\n([\s\S]*?)(?=\r?\n## |\s*$)/);
					const currentSteps = stepsMatch ? stepsMatch[1].trim() : body.trim();

					const repairedSteps = await repairSkillSteps(
						model,
						{
							skillDescription: frontmatter.description || "",
							currentSteps,
							negativeExamples: stats.negative_examples,
						},
						onUsage,
					);

					// Human approval gate: nothing is written or committed until the user
					// accepts. Skipped under autoRepair, which applies the rewrite unattended.
					let finalSteps = repairedSteps;
					let userEdited = false;
					const attemptNo = repairCount + 1;

					if (elicit?.interactive) {
						for (;;) {
							const choice = await elicit.select({
								title: `Proposed repair for skill "${params.skill_name}" (attempt ${attemptNo}/${MAX_REPAIRS})`,
								body: renderRepairPreview(currentSteps, finalSteps, stats.negative_examples),
								choices: [
									{ key: "a", label: "accept — write SKILL.md and commit" },
									{ key: "e", label: "edit — open the proposed steps in $EDITOR first" },
									{ key: "c", label: "cancel — leave the skill unchanged" },
								],
								defaultKey: "a",
								signal,
							});

							if (choice === "a") break;

							if (choice === "c") {
								return {
									content: [{
										type: "text",
										text: `Repair of "${params.skill_name}" was CANCELLED BY THE USER. ` +
											`skills/${params.skill_name}/SKILL.md is unchanged (confidence still ${stats.confidence}).\n\n` +
											`Do NOT retry the repair. Either use the skill as-is and report the real outcome, or solve the task from scratch.`,
									}],
									details: { skill_name: params.skill_name, approved: false, cancelled: true },
								};
							}

							const edited = await elicit.edit(finalSteps, { extension: ".md" });
							if (edited !== null) {
								finalSteps = edited.trim();
								userEdited = true;
							}
						}
					}

					const historyMatch = body.match(/## Repair History\r?\n([\s\S]*?)(?=\r?\n## |\s*$)/);
					const existingHistory = historyMatch ? historyMatch[1].trim() : "";
					const attempt = attemptNo;
					const lessonLines = stats.negative_examples.length
						? stats.negative_examples.map((n) => `  - ${n}`).join("\n")
						: "  - (no specific lessons recorded)";
					const approval = elicit?.interactive
						? userEdited ? " (user-edited, approved)" : " (user-approved)"
						: " (autoRepair)";
					const newEntry = `- Repair #${attempt} on ${new Date().toISOString()}${approval}:\n${lessonLines}`;
					const historyBody = existingHistory ? `${existingHistory}\n${newEntry}` : newEntry;

					frontmatter.confidence = REPAIR_RESET_CONFIDENCE;
					frontmatter.usage_count = 0;
					frontmatter.success_count = 0;
					frontmatter.failure_count = 0;
					frontmatter.negative_examples = [];
					frontmatter.repair_count = attempt;

					const newBody = `\n## Steps\n${finalSteps}\n\n## Repair History\n${historyBody}\n`;
					const yamlStr = yaml.dump(frontmatter, { lineWidth: -1, noRefs: true }).trimEnd();
					const updated = `---\n${yamlStr}\n---\n${newBody}`;

					await writeFile(skillFile, updated, "utf-8");
					gitCommit(agentDir, [`skills/${params.skill_name}/SKILL.md`], `Repair skill: ${params.skill_name}`);

					return {
						content: [{
							type: "text",
							text: `Skill "${params.skill_name}" repaired (attempt ${attempt}/${MAX_REPAIRS}) and committed.` +
								(elicit?.interactive
									? `\nThe user ${userEdited ? "edited and approved" : "approved"} the new steps.`
									: "") +
								`\nConfidence reset to ${REPAIR_RESET_CONFIDENCE} (was ${stats.confidence}).\nSteps rewritten based on ${stats.negative_examples.length} recorded failure(s).` +
								`\n\nNow load skills/${params.skill_name}/SKILL.md and follow the repaired steps.`,
						}],
						details: {
							skill_name: params.skill_name,
							repair_count: attempt,
							confidence: REPAIR_RESET_CONFIDENCE,
							approved: true,
							user_edited: userEdited,
						},
					};
				}

				case "update": {
					if (!params.skill_name) throw new Error("skill_name is required for update action");
					if (!params.instructions) throw new Error("instructions is required for update action");

					const skillFile = join(agentDir, "skills", params.skill_name, "SKILL.md");
					let content: string;
					try {
						content = await readFile(skillFile, "utf-8");
					} catch {
						throw new Error(`Skill not found: ${params.skill_name}`);
					}

					const fmMatch = content.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
					if (!fmMatch) throw new Error("Invalid SKILL.md format");

					const frontmatter = yaml.load(fmMatch[1]) as Record<string, any>;
					const yamlStr = yaml.dump(frontmatter, { lineWidth: -1, noRefs: true }).trimEnd();
					const updated = `---\n${yamlStr}\n---\n${params.instructions}\n`;

					await writeFile(skillFile, updated, "utf-8");
					gitCommit(agentDir, [`skills/${params.skill_name}/SKILL.md`], `Update skill: ${params.skill_name}`);

					return {
						content: [{ type: "text", text: `Skill "${params.skill_name}" updated and committed.` }],
						details: undefined,
					};
				}

				case "delete": {
					if (!params.skill_name) throw new Error("skill_name is required for delete action");

					const skillDir = join(agentDir, "skills", params.skill_name);
					try {
						await rm(skillDir, { recursive: true });
					} catch {
						throw new Error(`Skill not found: ${params.skill_name}`);
					}

					try {
						execSync(`git add -A && git commit -m "Delete skill: ${params.skill_name.replace(/"/g, '\\"')}"`, {
							cwd: agentDir,
							stdio: "pipe",
						});
					} catch {
						// Not fatal
					}

					return {
						content: [{ type: "text", text: `Skill "${params.skill_name}" deleted.` }],
						details: undefined,
					};
				}

				default:
					throw new Error(`Unknown action: ${params.action}`);
			}
		},
	};
}
