import { spawn } from "child_process";
import { access } from "fs/promises";
import { join } from "path";
import { Type } from "@sinclair/typebox";
import { StringEnum } from "@mariozechner/pi-ai";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { MAX_OUTPUT } from "./shared.js";

export const gitcronSchema = Type.Object({
	command: StringEnum(
		["schedule-create", "schedule-list", "task-create", "task-list", "task-update", "task-show", "remind-create", "remind-list", "remind-fire", "remind-pause", "remind-resume", "status", "validate", "generate"],
		{ description: "The gitcron command to run" },
	),
	name: Type.Optional(Type.String({ description: "Schedule/reminder name in kebab-case" })),
	cron: Type.Optional(Type.String({ description: "Cron expression (e.g., '0 2 * * *')" })),
	title: Type.Optional(Type.String({ description: "Task title or issue title" })),
	id: Type.Optional(Type.String({ description: "Task ID (e.g., TASK-001)" })),
	state: Type.Optional(Type.String({ description: "Task state (pending, in_progress, review, done, cancelled)" })),
	priority: Type.Optional(StringEnum(["low", "medium", "high"], { description: "Task priority" })),
	assignee: Type.Optional(Type.String({ description: "Assignee name" })),
	agent: Type.Optional(Type.String({ description: "Agent name for schedule" })),
	adapter: Type.Optional(StringEnum(["claude", "openai", "gitclaw", "system-prompt"], { description: "Adapter for agent execution" })),
	prompt: Type.Optional(Type.String({ description: "Prompt for agent schedule" })),
	shell_command: Type.Optional(Type.String({ description: "Shell command for command-type schedule" })),
	strategy: Type.Optional(StringEnum(["pr", "create", "commit", "none"], { description: "Branch strategy" })),
	body: Type.Optional(Type.String({ description: "Issue body for reminders" })),
	dry_run: Type.Optional(Type.Boolean({ description: "Preview without writing (for generate)" })),
});

interface GitcronArgs {
	command: string;
	name?: string;
	cron?: string;
	title?: string;
	id?: string;
	state?: string;
	priority?: string;
	assignee?: string;
	agent?: string;
	adapter?: string;
	prompt?: string;
	shell_command?: string;
	strategy?: string;
	body?: string;
	dry_run?: boolean;
}

function buildCliArgs(args: GitcronArgs): string[] {
	switch (args.command) {
		case "schedule-list":
			return ["list", "--schedules"];

		case "schedule-create":
			// Schedule creation is done by editing cron.yaml — we use the generate after
			return ["list", "--schedules"];

		case "task-create": {
			const cmd = ["task", "create", args.title || "Untitled"];
			if (args.priority) cmd.push("--priority", args.priority);
			if (args.assignee) cmd.push("--assignee", args.assignee);
			return cmd;
		}

		case "task-list": {
			const cmd = ["task", "list"];
			if (args.state) cmd.push("--state", args.state);
			return cmd;
		}

		case "task-update": {
			const cmd = ["task", "update", args.id || ""];
			if (args.state) cmd.push("--state", args.state);
			if (args.assignee) cmd.push("--assignee", args.assignee);
			return cmd;
		}

		case "task-show":
			return ["task", "show", args.id || ""];

		case "remind-create": {
			const cmd = ["remind", "create", args.name || ""];
			if (args.cron) cmd.push("--cron", args.cron);
			if (args.title) cmd.push("--title", args.title);
			if (args.body) cmd.push("--body", args.body);
			return cmd;
		}

		case "remind-list":
			return ["remind", "list"];

		case "remind-fire":
			return ["remind", "fire", args.name || ""];

		case "remind-pause":
			return ["remind", "pause", args.name || ""];

		case "remind-resume":
			return ["remind", "resume", args.name || ""];

		case "status":
			return ["status"];

		case "validate":
			return ["validate"];

		case "generate": {
			const cmd = ["generate"];
			if (args.dry_run) cmd.push("--dry-run");
			return cmd;
		}

		default:
			return ["--help"];
	}
}

export function createGitcronTool(cwd: string): AgentTool<typeof gitcronSchema> {
	return {
		name: "gitcron",
		label: "gitcron",
		description:
			"Git-native scheduling, tasks, and reminders. Create scheduled jobs (cron → GitHub Actions), manage tasks with state tracking, and set reminders. Commands: schedule-create/list, task-create/list/update/show, remind-create/list/fire/pause/resume, status, validate, generate.",
		parameters: gitcronSchema,
		execute: async (
			_toolCallId: string,
			args: GitcronArgs,
			signal?: AbortSignal,
		) => {
			if (signal?.aborted) throw new Error("Operation aborted");

			// Check if gitcron is available
			const cliArgs = buildCliArgs(args);

			return new Promise((resolve, reject) => {
				const child = spawn("gitcron", cliArgs, {
					cwd,
					stdio: ["ignore", "pipe", "pipe"],
					env: { ...process.env },
				});

				let output = "";

				child.stdout.on("data", (data: Buffer) => {
					output += data.toString("utf-8");
				});
				child.stderr.on("data", (data: Buffer) => {
					output += data.toString("utf-8");
				});

				const timeout = setTimeout(() => {
					child.kill("SIGTERM");
					reject(new Error("gitcron command timed out after 30s"));
				}, 30_000);

				const onAbort = () => child.kill("SIGTERM");
				if (signal) signal.addEventListener("abort", onAbort, { once: true });

				child.on("error", (err) => {
					clearTimeout(timeout);
					if (signal) signal.removeEventListener("abort", onAbort);
					reject(new Error(`gitcron not found. Install with: npm install -g gitcron. Error: ${err.message}`));
				});

				child.on("close", (code) => {
					clearTimeout(timeout);
					if (signal) signal.removeEventListener("abort", onAbort);

					if (signal?.aborted) {
						reject(new Error("Operation aborted"));
						return;
					}

					const trimmed = output.trim();
					const text = trimmed.length > MAX_OUTPUT
						? trimmed.slice(-MAX_OUTPUT)
						: trimmed || "(no output)";

					resolve({
						content: [{ type: "text", text }],
						details: { exitCode: code },
					});
				});
			});
		},
	};
}
