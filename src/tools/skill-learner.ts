import { readFile, writeFile, mkdir, readdir, rm } from "fs/promises";
import { join } from "path";
import { execSync } from "child_process";
import { type Static } from "@sinclair/typebox";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { skillLearnerSchema } from "./shared.js";
import { loadSkillStats, isSkillFlagged } from "../learning/reinforcement.js";
import type { TaskRecord } from "./task-tracker.js";
import yaml from "js-yaml";

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

export function createSkillLearnerTool(agentDir: string, gitagentDir: string): AgentTool<typeof skillLearnerSchema> {
	return {
		name: "skill_learner",
		label: "skill_learner",
		description:
			"Learn from successful tasks. Use 'evaluate' to check if a completed task is worth saving as a skill, 'crystallize' to save it, 'status' to list all skills with confidence scores, 'review' to see flagged low-confidence skills, 'update' to modify a skill, 'delete' to remove one.",
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

					const skills: Array<{ name: string; confidence: number; usage: number; ratio: string }> = [];

					for (const entry of entries) {
						if (!entry.isDirectory()) continue;
						const dir = join(skillsDir, entry.name);
						const stats = await loadSkillStats(dir);
						// Only include learned skills (those with stats fields)
						skills.push({
							name: entry.name,
							confidence: stats.confidence,
							usage: stats.usage_count,
							ratio: `${stats.success_count}/${stats.success_count + stats.failure_count}`,
						});
					}

					if (skills.length === 0) {
						return {
							content: [{ type: "text", text: "No skills found." }],
							details: undefined,
						};
					}

					const lines = skills.map((s) =>
						`  ${s.name}: confidence=${s.confidence}, usage=${s.usage}, success_ratio=${s.ratio}`,
					);
					return {
						content: [{ type: "text", text: `Skills:\n${lines.join("\n")}` }],
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
						content: [{ type: "text", text: `Flagged skills (confidence < 0.4):\n${lines.join("\n")}\n\nConsider updating or deleting these skills.` }],
						details: { flagged },
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
